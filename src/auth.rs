//! 核心流程：定位浏览器 → 必要时关闭 → 直接在原始 profile 上无头读会话 → 生成 auth.json
//!
//! 为什么不拷贝 profile：新版本 Edge/Chrome 的 Cookie 采用 v20 App-Bound 加密，
//! 密钥绑定原始 user-data-dir；拷到临时目录后无头浏览器解不开这些 Cookie，
//! 还会把它们当损坏数据删掉，导致永远读不到会话。所以直接在原目录上跑
//! 无头浏览器（跑之前先优雅关闭浏览器解锁，读完再恢复）。

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------
// 进度
// ---------------------------------------------------------------------------

pub const P_INFO: u8 = 0;
pub const P_STEP: u8 = 1;
pub const P_OK: u8 = 2;
pub const P_WARN: u8 = 3;

pub type Prog<'a> = &'a dyn Fn(u8, &str);

// ---------------------------------------------------------------------------
// 数据结构
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct Browser {
    pub name: String,
    pub exe: String,
    pub udd: String,
}

#[derive(Debug, Clone, Default)]
pub struct Outcome {
    pub email: String,
    pub plan: String,
    pub expires: String,
    pub left: String,
    pub path: String,
    pub backup: String,
    pub browser: String,
    pub profile: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct SessionUser {
    #[serde(default, deserialize_with = "null_to_empty")]
    id: String,
    #[serde(default, deserialize_with = "null_to_empty")]
    email: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct SessionAccount {
    #[serde(default, deserialize_with = "null_to_empty")]
    id: String,
    #[serde(default, rename = "planType", deserialize_with = "null_to_empty")]
    plan_type: String,
}

/// 接口里这些字段可能给 null，统一按空串处理
fn null_to_empty<'de, D>(d: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<String>::deserialize(d)?.unwrap_or_default())
}

#[derive(Debug, Clone, Default, Deserialize)]
struct Session {
    #[serde(default, rename = "accessToken", deserialize_with = "null_to_empty")]
    access_token: String,
    #[serde(default, rename = "sessionToken", deserialize_with = "null_to_empty")]
    session_token: String,
    #[serde(default)]
    expires: Value,
    #[serde(default)]
    user: SessionUser,
    #[serde(default)]
    account: SessionAccount,
}

impl Session {
    fn expires_str(&self) -> String {
        match &self.expires {
            Value::String(s) => s.clone(),
            Value::Number(n) => n.to_string(),
            _ => String::new(),
        }
    }
}

#[derive(Serialize)]
struct Tokens {
    id_token: String,
    access_token: String,
    refresh_token: String,
    account_id: String,
}

#[derive(Serialize)]
struct AuthFile {
    auth_mode: &'static str,
    #[serde(rename = "OPENAI_API_KEY")]
    openai_api_key: Option<()>,
    tokens: Tokens,
    last_refresh: String,
}

#[derive(Serialize)]
struct JwtHeader {
    alg: &'static str,
    typ: &'static str,
    cpa_synthetic: bool,
}

#[derive(Serialize)]
struct AuthBlock {
    chatgpt_account_id: String,
    chatgpt_plan_type: String,
    chatgpt_user_id: String,
    user_id: String,
}

#[derive(Serialize)]
struct JwtPayload {
    iat: i64,
    exp: i64,
    #[serde(rename = "https://api.openai.com/auth")]
    auth: AuthBlock,
    email: String,
}

// ---------------------------------------------------------------------------
// 路径工具
// ---------------------------------------------------------------------------

/// 展开 `%VAR%` 形式的 Windows 环境变量，未定义时原样保留。
#[cfg(windows)]
fn expand_env(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(p) = rest.find('%') {
        let after = &rest[p + 1..];
        match after.find('%') {
            Some(q) => {
                let key = &after[..q];
                out.push_str(&rest[..p]);
                match std::env::var(key) {
                    Ok(v) if !v.is_empty() => out.push_str(&v),
                    _ => {
                        out.push('%');
                        out.push_str(key);
                        out.push('%');
                    }
                }
                rest = &after[q + 1..];
            }
            None => {
                out.push_str(rest);
                return out;
            }
        }
    }
    out.push_str(rest);
    out
}

fn first_existing(paths: &[String]) -> Option<String> {
    for p in paths {
        if fs::metadata(p).map(|m| !m.is_dir()).unwrap_or(false) {
            return Some(p.clone());
        }
    }
    None
}

pub fn find_browsers() -> Vec<Browser> {
    // (名称, 可执行文件候选, 用户数据目录)。win 用 %VAR% 展开；
    // mac 的 exe 在 .app 包内；linux 用 PATH 里的命令名，udd 在 ~/.config
    #[cfg(windows)]
    let list: Vec<(&'static str, Vec<&'static str>, &'static str)> = vec![
        (
            "Chrome",
            vec![
                r"%ProgramFiles%\Google\Chrome\Application\chrome.exe",
                r"%ProgramFiles(x86)%\Google\Chrome\Application\chrome.exe",
                r"%LOCALAPPDATA%\Google\Chrome\Application\chrome.exe",
            ],
            r"%LOCALAPPDATA%\Google\Chrome\User Data",
        ),
        (
            "Edge",
            vec![
                r"%ProgramFiles%\Microsoft\Edge\Application\msedge.exe",
                r"%ProgramFiles(x86)%\Microsoft\Edge\Application\msedge.exe",
                r"%LOCALAPPDATA%\Microsoft\Edge\Application\msedge.exe",
            ],
            r"%LOCALAPPDATA%\Microsoft\Edge\User Data",
        ),
        (
            "Brave",
            vec![
                r"%ProgramFiles%\BraveSoftware\Brave-Browser\Application\brave.exe",
                r"%ProgramFiles(x86)%\BraveSoftware\Brave-Browser\Application\brave.exe",
            ],
            r"%LOCALAPPDATA%\BraveSoftware\Brave-Browser\User Data",
        ),
        (
            "Chromium",
            vec![
                r"%ProgramFiles%\Chromium\Application\chrome.exe",
                r"%LOCALAPPDATA%\Chromium\Application\chrome.exe",
            ],
            r"%LOCALAPPDATA%\Chromium\User Data",
        ),
    ];

    #[cfg(not(windows))]
    let list: Vec<(&'static str, Vec<&'static str>, String)> = {
        #[cfg(target_os = "macos")]
        let base = "~/Library/Application Support";
        #[cfg(not(target_os = "macos"))]
        let base = "~/.config";
        let mk = |rel: &str| format!("{}/{}", base, rel);
        vec![
            (
                "Chrome",
                vec![
                    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
                    "google-chrome",
                    "google-chrome-stable",
                    "/usr/bin/google-chrome",
                    "/usr/bin/google-chrome-stable",
                ],
                mk("Google/Chrome"),
            ),
            (
                "Edge",
                vec![
                    "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
                    "microsoft-edge",
                    "microsoft-edge-stable",
                    "/usr/bin/microsoft-edge",
                    "/usr/bin/microsoft-edge-stable",
                ],
                mk("Microsoft Edge"),
            ),
            (
                "Brave",
                vec![
                    "/Applications/Brave Browser.app/Contents/MacOS/Brave Browser",
                    "brave-browser",
                    "/usr/bin/brave-browser",
                ],
                mk("BraveSoftware/Brave-Browser"),
            ),
            (
                "Chromium",
                vec![
                    "/Applications/Chromium.app/Contents/MacOS/Chromium",
                    "chromium",
                    "chromium-browser",
                    "/usr/bin/chromium",
                ],
                mk("Chromium"),
            ),
        ]
    };

    let expand = |s: &str| -> String {
        #[cfg(windows)]
        {
            expand_env(s)
        }
        #[cfg(not(windows))]
        {
            // ~ 展开为家目录；其余原样（命令名靠 PATH 解析）
            if let Some(rest) = s.strip_prefix("~/") {
                if let Ok(h) = std::env::var("HOME") {
                    return Path::new(&h).join(rest).display().to_string();
                }
            }
            s.to_string()
        }
    };

    let mut out = Vec::new();
    for (name, exes, udd) in list.iter() {
        let exes: Vec<String> = exes.iter().map(|e| expand(e)).collect();
        if let Some(exe) = first_existing(&exes) {
            let udd = expand(udd);
            if fs::metadata(&udd).map(|m| m.is_dir()).unwrap_or(false) {
                out.push(Browser {
                    name: name.to_string(),
                    exe,
                    udd,
                });
            }
        }
    }
    out
}

pub fn list_profiles(udd: &str) -> Vec<String> {
    let mut profiles: Vec<String> = Vec::new();
    if let Ok(rd) = fs::read_dir(udd) {
        for e in rd.flatten() {
            if !e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                continue;
            }
            let name = e.file_name().to_string_lossy().to_string();
            let cookies = Path::new(udd).join(&name).join("Network").join("Cookies");
            if fs::metadata(&cookies).map(|m| !m.is_dir()).unwrap_or(false) {
                profiles.push(name);
            }
        }
    }
    profiles.sort();

    // 把上次使用 / Default 提到最前面
    let mut last_used = String::new();
    if let Ok(b) = fs::read(Path::new(udd).join("Local State")) {
        if let Ok(v) = serde_json::from_slice::<Value>(&b) {
            if let Some(s) = v.pointer("/profile/last_used").and_then(|x| x.as_str()) {
                last_used = s.to_string();
            }
        }
    }
    let move_front = |v: &mut Vec<String>, name: &str| {
        if let Some(i) = v.iter().position(|p| p == name) {
            let item = v.remove(i);
            v.insert(0, item);
        }
    };
    if !last_used.is_empty() {
        move_front(&mut profiles, &last_used);
    }
    if fs::metadata(Path::new(udd).join("Default")).is_ok() {
        move_front(&mut profiles, "Default");
    }
    profiles
}

// ---------------------------------------------------------------------------
// 进程工具
// ---------------------------------------------------------------------------

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

fn hidden(cmd: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
}

/// 运行并捕获输出。`timeout` 为 `None` 时不限时，一直等到子进程自己退出；
/// 有限时则到点杀掉子进程。
///
/// 用共享缓冲 + 独立读线程，而不是 join 读线程：浏览器会派生子进程，
/// 它们可能继承管道句柄，父进程被杀后管道仍不关闭，join 会永久挂住。
fn run_capture(mut cmd: Command, timeout: Option<Duration>) -> Option<String> {
    hidden(&mut cmd);
    let mut child = cmd.spawn().ok()?;

    let buf_out = Arc::new(Mutex::new(Vec::<u8>::new()));
    let buf_err = Arc::new(Mutex::new(Vec::<u8>::new()));
    let finished = Arc::new(AtomicUsize::new(0));

    let mut readers = Vec::new();
    if let Some(pipe) = child.stdout.take() {
        readers.push(spawn_reader(Box::new(pipe), buf_out.clone(), finished.clone()));
    }
    if let Some(pipe) = child.stderr.take() {
        readers.push(spawn_reader(Box::new(pipe), buf_err.clone(), finished.clone()));
    }
    let want = readers.len();

    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if let Some(t) = timeout {
                    if start.elapsed() > t {
                        let _ = child.kill();
                        let _ = child.wait();
                        break;
                    }
                }
                std::thread::sleep(Duration::from_millis(60));
            }
            Err(_) => break,
        }
    }

    // 给读线程一点时间把剩余数据刷完，拿不到也不强等
    let drain_deadline = Instant::now() + Duration::from_secs(2);
    while finished.load(Ordering::SeqCst) < want && Instant::now() < drain_deadline {
        std::thread::sleep(Duration::from_millis(20));
    }

    let mut all = buf_out.lock().map(|g| g.clone()).unwrap_or_default();
    if let Ok(g) = buf_err.lock() {
        all.extend_from_slice(&g);
    }
    Some(String::from_utf8_lossy(&all).to_string())
}

fn spawn_reader(
    mut pipe: Box<dyn Read + Send>,
    buf: Arc<Mutex<Vec<u8>>>,
    finished: Arc<AtomicUsize>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut tmp = [0u8; 8192];
        loop {
            match pipe.read(&mut tmp) {
                Ok(0) => break,
                Ok(n) => {
                    if let Ok(mut g) = buf.lock() {
                        g.extend_from_slice(&tmp[..n]);
                    }
                }
                Err(_) => break,
            }
        }
        finished.fetch_add(1, Ordering::SeqCst);
    })
}

fn can_read(path: &Path) -> bool {
    match fs::File::open(path) {
        Ok(mut f) => {
            let mut buf = [0u8; 1];
            f.read(&mut buf).is_ok()
        }
        Err(_) => false,
    }
}

/// 优雅关闭浏览器（等同点窗口关闭），必要时强制结束。
/// 成功判据是 Cookie 库真正解锁，而不是进程数归零。
fn close_browser(exe_name: &str, lock: &Path, prog: Prog) -> bool {
    if can_read(lock) {
        return true;
    }
    let pretty = exe_name.trim_end_matches(".exe");
    prog(P_STEP, &format!("正在关闭 {}…", pretty));

    // unix 上浏览器不独占锁 Cookie 文件，一般走到这里就说明真的锁了；
    // win 用 taskkill，unix 用 pkill
    #[cfg(windows)]
    fn killer(exe: &str, force: bool) -> Command {
        let mut c = Command::new("taskkill");
        if force {
            c.args(["/F", "/T", "/IM", exe]);
        } else {
            c.args(["/IM", exe]);
        }
        c
    }
    #[cfg(not(windows))]
    fn killer(exe: &str, force: bool) -> Command {
        // exe 可能是完整路径，取最后一段做进程名
        let name = Path::new(exe)
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| exe.to_string());
        let mut c = Command::new("pkill");
        if force {
            c.args(["-9", "-x", &name]);
        } else {
            c.args(["-TERM", "-x", &name]);
        }
        c
    }

    let _ = run_capture(killer(exe_name, false), Some(Duration::from_secs(10)));

    let deadline = Instant::now() + Duration::from_secs(8);
    while Instant::now() < deadline {
        if can_read(lock) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(400));
    }

    prog(P_INFO, "浏览器仍在后台驻留，强制结束");
    let _ = run_capture(killer(exe_name, true), Some(Duration::from_secs(15)));

    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        if can_read(lock) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(300));
    }
    can_read(lock)
}

// ---------------------------------------------------------------------------
// 读取会话
// ---------------------------------------------------------------------------

const SESSION_URL: &str = "https://chatgpt.com/api/auth/session";
const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
                          (KHTML, like Gecko) Chrome/156.0.0.0 Safari/537.36";

/// 直接在原始 user-data-dir 上无头读取（新 Edge/Chrome 的 Cookie 是 v20
/// App-Bound 加密，密钥绑定原始数据目录；拷出去的 Cookies 解不开会被
/// 浏览器当作损坏删掉，所以不能再走「拷贝 profile」的老路）。
///
/// 不设超时：一直等到浏览器自己退出（--dump-dom 模式下读完即退）。
fn dump_dom(exe: &str, udd: &str, profile: &str, extra: &[&str]) -> String {
    let mut cmd = Command::new(exe);
    cmd.args([
        "--headless=new",
        "--disable-gpu",
        "--no-first-run",
        "--no-default-browser-check",
        "--disable-blink-features=AutomationControlled",
        "--disable-features=Translate,MediaRouter,OptimizationHints",
        "--disable-background-networking",
    ]);
    cmd.arg(format!("--user-agent={}", USER_AGENT));
    cmd.arg(format!("--user-data-dir={}", udd));
    cmd.arg(format!("--profile-directory={}", profile));
    cmd.args(extra);
    cmd.arg("--dump-dom").arg(SESSION_URL);

    run_capture(cmd, None).unwrap_or_default()
}

fn unescape_html(text: &str) -> String {
    text.replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

/// 从 dump-dom 输出里抠出第一个合法 JSON 对象
fn extract_json(text: &str) -> Option<Value> {
    let text = unescape_html(text);
    let bytes = text.as_bytes();
    for i in 0..bytes.len() {
        if bytes[i] != b'{' {
            continue;
        }
        let mut depth = 0i32;
        for j in i..bytes.len() {
            match bytes[j] {
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        if let Ok(v) = serde_json::from_str::<Value>(&text[i..=j]) {
                            return Some(v);
                        }
                        break;
                    }
                }
                _ => {}
            }
        }
    }
    None
}

fn fetch_session(exe: &str, udd: &str, profile: &str) -> Result<Session, String> {
    let variants: [&[&str]; 3] = [
        &[],
        &["--virtual-time-budget=20000"],
        &[
            "--virtual-time-budget=30000",
            "--disable-3d-apis",
            "--no-sandbox",
        ],
    ];
    let mut last_err = "未找到登录会话（该浏览器未登录 chatgpt.com）".to_string();

    for extra in variants.iter() {
        let out = dump_dom(exe, udd, profile, extra);
        if out.trim().is_empty() {
            last_err = "浏览器未返回任何内容".to_string();
            continue;
        }
        if out.contains("accessToken") {
            if let Some(v) = extract_json(&out) {
                if let Ok(s) = serde_json::from_value::<Session>(v) {
                    if !s.access_token.is_empty() {
                        return Ok(s);
                    }
                }
            }
            last_err = "接口未返回有效 accessToken".into();
            continue;
        }
        let low = out.to_lowercase();
        if low.contains("just a moment")
            || low.contains("cf-challenge")
            || low.contains("cf-browser-verification")
        {
            last_err = "被 Cloudflare 拦截".into();
        } else if low.contains("unauthorized") {
            last_err = "会话已失效，需要重新登录".into();
        }
    }
    Err(last_err)
}

// ---------------------------------------------------------------------------
// 组装 auth.json
// ---------------------------------------------------------------------------

const B64URL: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

fn b64url(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(B64URL[((n >> 18) & 63) as usize] as char);
        out.push(B64URL[((n >> 12) & 63) as usize] as char);
        if chunk.len() > 1 {
            out.push(B64URL[((n >> 6) & 63) as usize] as char);
        }
        if chunk.len() > 2 {
            out.push(B64URL[(n & 63) as usize] as char);
        }
    }
    out
}

fn b64url_json<T: Serialize>(v: &T) -> String {
    b64url(&serde_json::to_vec(v).unwrap_or_default())
}

/// base64url 解码（容忍缺省 padding）
fn b64url_decode(s: &str) -> Option<Vec<u8>> {
    let mut vals = Vec::with_capacity(s.len());
    for c in s.bytes() {
        let idx = match c {
            b'A'..=b'Z' => (c - b'A') as u32,
            b'a'..=b'z' => (c - b'a' + 26) as u32,
            b'0'..=b'9' => (c - b'0' + 52) as u32,
            b'-' => 62,
            b'_' => 63,
            b'=' => continue,
            _ => return None,
        };
        vals.push(idx);
    }
    let mut out = Vec::with_capacity(vals.len() * 3 / 4 + 3);
    for chunk in vals.chunks(4) {
        let k = chunk.len();
        if k < 2 {
            break;
        }
        let n = chunk.iter().fold(0u32, |acc, &v| (acc << 6) | v);
        // k 个 sextet 承载 k-1 个字节：从高位按 8 位切出
        let shift = (6 * k - 8) as u32;
        for i in 0..k - 1 {
            out.push(((n >> (shift - 8 * i as u32)) & 0xFF) as u8);
        }
    }
    Some(out)
}

/// 解出 JWT payload 里的 exp（Unix 秒）
fn jwt_payload_exp(jwt: &str) -> Option<i64> {
    jwt_payload_claim(jwt, "exp")
}

/// 解出 JWT payload 里的 iat（Unix 秒）
fn jwt_payload_iat(jwt: &str) -> Option<i64> {
    jwt_payload_claim(jwt, "iat")
}

fn jwt_payload_claim(jwt: &str, claim: &str) -> Option<i64> {
    let payload = jwt.split('.').nth(1)?;
    let bytes = b64url_decode(payload)?;
    let v: Value = serde_json::from_slice(&bytes).ok()?;
    v.get(claim)?.as_i64()
}

/// JWT exp 转成 RFC3339，供 fmt_local / human_left 使用
fn jwt_exp_iso(jwt: &str) -> Option<String> {
    let exp = jwt_payload_exp(jwt)?;
    chrono::DateTime::from_timestamp(exp, 0).map(|t| t.with_timezone(&chrono::Utc).to_rfc3339())
}

fn parse_rfc3339(s: &str) -> Option<chrono::DateTime<chrono::FixedOffset>> {
    chrono::DateTime::parse_from_rfc3339(s).ok()
}

fn build_auth(s: &Session) -> AuthFile {
    let now = chrono::Local::now();
    let expires_iso = s.expires_str();
    let exp = parse_rfc3339(&expires_iso)
        .map(|t| t.with_timezone(&chrono::Local))
        .unwrap_or_else(|| now + chrono::Duration::days(30));

    let header = JwtHeader {
        alg: "none",
        typ: "JWT",
        cpa_synthetic: true,
    };
    let payload = JwtPayload {
        iat: now.timestamp(),
        exp: exp.timestamp(),
        auth: AuthBlock {
            chatgpt_account_id: s.account.id.clone(),
            chatgpt_plan_type: s.account.plan_type.clone(),
            chatgpt_user_id: s.user.id.clone(),
            user_id: s.user.id.clone(),
        },
        email: s.user.email.clone(),
    };
    let id_token = format!(
        "{}.{}.synthetic",
        b64url_json(&header),
        b64url_json(&payload)
    );

    let refresh = if s.session_token.is_empty() {
        "placeholder".to_string()
    } else {
        s.session_token.clone()
    };

    AuthFile {
        auth_mode: "chatgpt",
        openai_api_key: None,
        tokens: Tokens {
            id_token,
            access_token: s.access_token.clone(),
            refresh_token: refresh,
            account_id: s.account.id.clone(),
        },
        last_refresh: now
            .with_timezone(&chrono::Utc)
            .format("%Y-%m-%dT%H:%M:%S%.3fZ")
            .to_string(),
    }
}

pub fn codex_home() -> PathBuf {
    if let Some(h) = std::env::var_os("CODEX_HOME") {
        if !h.is_empty() {
            return PathBuf::from(h);
        }
    }
    let home = std::env::var("USERPROFILE")
        .ok()
        .or_else(|| std::env::var("HOME").ok())
        .unwrap_or_else(|| ".".into());
    PathBuf::from(home).join(".codex")
}

fn write_auth(auth: &AuthFile, do_backup: bool) -> Result<(String, String), String> {
    let dir = codex_home();
    fs::create_dir_all(&dir).map_err(|e| format!("无法创建 {}: {}", dir.display(), e))?;
    let target = dir.join("auth.json");
    let data = serde_json::to_vec_pretty(auth).map_err(|e| e.to_string())?;

    let mut backup = String::new();
    if do_backup && target.exists() {
        let p = dir.join(format!(
            "auth.json.bak-{}",
            chrono::Local::now().format("%Y%m%d-%H%M%S")
        ));
        if fs::copy(&target, &p).is_ok() {
            backup = p.display().to_string();
        }
    }

    fs::write(&target, &data).map_err(|e| format!("写入失败: {}", e))?;

    // 只允许当前用户访问
    #[cfg(windows)]
    if let Ok(user) = std::env::var("USERNAME") {
        let target_s = target.to_string_lossy().to_string();
        let grant = format!("{}:F", user);
        let mut c = Command::new("icacls");
        c.args([target_s.as_str(), "/inheritance:r", "/grant:r", grant.as_str()]);
        let _ = run_capture(c, Some(Duration::from_secs(10)));
    }
    #[cfg(not(windows))]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&target, fs::Permissions::from_mode(0o600));
    }

    Ok((target.display().to_string(), backup))
}

// ---------------------------------------------------------------------------
// 展示用格式化
// ---------------------------------------------------------------------------

pub fn fmt_local(iso: &str) -> String {
    if iso.is_empty() {
        return "未知".into();
    }
    match parse_rfc3339(iso) {
        Some(t) => t
            .with_timezone(&chrono::Local)
            .format("%Y-%m-%d %H:%M")
            .to_string(),
        None => iso.to_string(),
    }
}

pub fn human_left(iso: &str) -> String {
    let t = match parse_rfc3339(iso) {
        Some(t) => t,
        None => return String::new(),
    };
    let d = t.with_timezone(&chrono::Local) - chrono::Local::now();
    if d.num_seconds() <= 0 {
        return "已过期".into();
    }
    let total_hours = d.num_hours();
    let days = total_hours / 24;
    let hours = total_hours % 24;
    let mins = d.num_minutes() % 60;
    let mut s = String::new();
    if days > 0 {
        s.push_str(&format!("{}天", days));
    }
    if hours > 0 || days > 0 {
        s.push_str(&format!("{}小时", hours));
    }
    s.push_str(&format!("{}分", mins));
    s
}

// ---------------------------------------------------------------------------
// 主流程
// ---------------------------------------------------------------------------

/// 读取失败时的处理建议
fn failure_hint(err: &str) -> String {
    format!(
        "{err}\n\
         建议：\n\
         1. 确认已在该浏览器登录 chatgpt.com，且页面能正常打开\n\
         2. 会话过期就退出重新登录后再试\n\
         3. 点「重试」再跑一次；Edge 反复失败就换 Chrome 试试"
    )
}

/// 把默认浏览器的标识（ProgId / bundle id / desktop 文件名）映射到内部名称
fn map_default_browser(id: &str) -> String {
    let pid = id.to_lowercase();
    const MAP: &[(&str, &str)] = &[
        ("chromehtml", "Chrome"),
        ("com.google.chrome", "Chrome"),
        ("google-chrome", "Chrome"),
        ("msedgehtm", "Edge"),
        ("com.microsoft.edgemac", "Edge"),
        ("microsoft-edge", "Edge"),
        ("brave", "Brave"),
        ("com.brave", "Brave"),
        ("chromiumhtml", "Chromium"),
        ("org.chromium.chromium", "Chromium"),
        ("chromium", "Chromium"),
    ];
    for (k, name) in MAP {
        if pid.contains(k) {
            return name.to_string();
        }
    }
    String::new()
}

/// 读取系统默认浏览器。读不到或不支持时返回空串。
#[cfg(windows)]
pub fn default_browser_name() -> String {
    let mut c = Command::new("reg");
    c.args([
        "query",
        r"HKCU\Software\Microsoft\Windows\Shell\Associations\UrlAssociations\https\UserChoice",
        "/v",
        "ProgId",
    ]);
    let out = run_capture(c, Some(Duration::from_secs(5))).unwrap_or_default();
    let pid = out
        .lines()
        .map(str::trim)
        .find(|l| l.starts_with("ProgId"))
        .and_then(|l| l.split_whitespace().last())
        .unwrap_or("");
    map_default_browser(pid)
}

/// mac：从 LaunchServices 配置里找 https 的默认处理程序 bundle id
#[cfg(target_os = "macos")]
pub fn default_browser_name() -> String {
    let mut c = Command::new("defaults");
    c.args([
        "read",
        "com.apple.LaunchServices/com.apple.launchservices.secure",
        "LSHandlers",
    ]);
    let out = run_capture(c, Some(Duration::from_secs(5))).unwrap_or_default();
    // 找到包含 https 的条目，取其后最近的 LSHandlerRoleAgent / LSHandlerURLScheme 之后的 bundle id
    let mut hit = false;
    for line in out.lines() {
        let l = line.trim();
        if l.contains("https") {
            hit = true;
            continue;
        }
        if hit {
            if let Some(id) = l
                .strip_prefix("LSHandlerRoleAgent = \"")
                .or_else(|| l.strip_prefix("LSHandlerRoleAll = \""))
            {
                let id = id.trim_end_matches("\";");
                if !id.is_empty() && id != "-" {
                    return map_default_browser(id);
                }
            }
            if l.starts_with("LSHandlerURLScheme") || l.starts_with("}") {
                hit = false;
            }
        }
    }
    String::new()
}

/// Linux：xdg-settings 查询默认浏览器
#[cfg(all(unix, not(target_os = "macos")))]
pub fn default_browser_name() -> String {
    let mut c = Command::new("xdg-settings");
    c.args(["get", "default-web-browser"]);
    let out = run_capture(c, Some(Duration::from_secs(5))).unwrap_or_default();
    map_default_browser(out.trim())
}

/// 执行完整流程。`browser_name` 为空表示自动（依次尝试）；
/// `do_backup` 为 false 时不备份旧 auth.json，直接覆盖。
pub fn run(browser_name: &str, do_backup: bool, prog: Prog) -> Result<Outcome, String> {
    let mut browsers = find_browsers();
    if !browser_name.is_empty() {
        browsers.retain(|b| b.name.eq_ignore_ascii_case(browser_name));
    }
    if browsers.is_empty() {
        return Err("没有找到可用的 Chromium 系浏览器".into());
    }

    let mut last_err: Option<String> = None;
    for b in browsers.iter() {
        let mut profiles = list_profiles(&b.udd);
        if profiles.is_empty() {
            continue;
        }
        let exe_name = Path::new(&b.exe)
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        let lock = Path::new(&b.udd)
            .join(&profiles[0])
            .join("Network")
            .join("Cookies");

        let need_close = !can_read(&lock);
        if need_close && !close_browser(&exe_name, &lock, prog) {
            prog(P_WARN, &format!("{} 无法关闭，跳过", b.name));
            continue;
        }

        profiles = list_profiles(&b.udd);
        let mut got: Option<(String, Session)> = None;
        for (i, profile) in profiles.iter().enumerate() {
            if i >= 4 {
                break;
            }
            prog(
                P_STEP,
                &format!("正在读取 {} / {} 的会话…", b.name, profile),
            );
            match fetch_session(&b.exe, &b.udd, profile) {
                Ok(sess) => {
                    got = Some((profile.clone(), sess));
                    break;
                }
                Err(e) => {
                    prog(P_WARN, &format!("{}: {}", profile, e));
                    last_err = Some(e);
                }
            }
        }

        if need_close {
            prog(
                P_OK,
                &format!("{} 已关闭（读取会话需要解锁 Cookie 库），可自行重新打开", b.name),
            );
        }

        if let Some((profile, sess)) = got {
            let auth = build_auth(&sess);
            let (target, backup) = write_auth(&auth, do_backup)?;
            // access_token 的 JWT exp 通常比会话 expires 短得多，
            // 才是 Codex 实际可用的窗口 —— 展示以它为准
            let exp_iso = jwt_exp_iso(&sess.access_token).unwrap_or_else(|| sess.expires_str());
            // session 端点会一直返回缓存的旧 token：若签发时间太久，多半已被
            // 服务端轮换吊销（Codex 会 401 → 尝试刷新 → 失败），提醒用户重新登录
            if let Some(iat) = jwt_payload_iat(&sess.access_token) {
                let age_days = (chrono::Utc::now().timestamp() - iat) / 86400;
                if age_days >= 3 {
                    prog(
                        P_WARN,
                        &format!(
                            "该 access_token 是 {} 天前签发的缓存。若 Codex 报「access token 无法刷新」，\
                             请先在浏览器退出并重新登录 chatgpt.com，再重新导出",
                            age_days
                        ),
                    );
                }
            }
            return Ok(Outcome {
                email: sess.user.email.clone(),
                plan: if sess.account.plan_type.is_empty() {
                    "FREE".into()
                } else {
                    sess.account.plan_type.to_uppercase()
                },
                expires: fmt_local(&exp_iso),
                left: human_left(&exp_iso),
                path: target,
                backup,
                browser: b.name.clone(),
                profile,
            });
        }
    }
    match last_err {
        Some(e) => Err(failure_hint(&e)),
        None => Err(failure_hint("没能读到 ChatGPT 登录会话")),
    }
}

// ---------------------------------------------------------------------------
// 设置记忆
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default)]
    pub last_browser: String,
    /// 是否在覆盖 auth.json 前备份旧文件，默认开
    #[serde(default = "default_true")]
    pub backup: bool,
}

fn default_true() -> bool {
    true
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            last_browser: String::new(),
            backup: true,
        }
    }
}

fn settings_path() -> PathBuf {
    #[cfg(windows)]
    let base = std::env::var("APPDATA").unwrap_or_else(|_| std::env::temp_dir().display().to_string());
    #[cfg(not(windows))]
    let base = std::env::var("HOME")
        .map(|h| Path::new(&h).join(".config").display().to_string())
        .unwrap_or_else(|_| std::env::temp_dir().display().to_string());
    PathBuf::from(base)
        .join("CodexAuthHelper")
        .join("settings.json")
}

pub fn load_settings() -> Settings {
    fs::read(settings_path())
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

pub fn save_settings(s: &Settings) {
    let p = settings_path();
    if let Some(d) = p.parent() {
        let _ = fs::create_dir_all(d);
    }
    if let Ok(b) = serde_json::to_vec_pretty(s) {
        let _ = fs::write(p, b);
    }
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64url_vectors() {
        assert_eq!(b64url(b""), "");
        assert_eq!(b64url(b"f"), "Zg");
        assert_eq!(b64url(b"fo"), "Zm8");
        assert_eq!(b64url(b"foo"), "Zm9v");
        assert_eq!(b64url(b"foob"), "Zm9vYg");
        assert_eq!(b64url(&[0xfb, 0xff]), "-_8");
    }

    #[test]
    fn json_extraction_handles_entities_and_nesting() {
        let html = r#"<html><body><pre>{"a":&quot;x&quot;,"b":{"c":[1,2]}}</pre></body></html>"#;
        let v = extract_json(html).expect("应当能解析");
        assert_eq!(v["a"], "x");
        assert_eq!(v["b"]["c"][1], 2);
    }

    #[test]
    fn json_extraction_skips_garbage() {
        assert!(extract_json("no json here").is_none());
        assert!(extract_json("{not json}").is_none());
    }

    #[test]
    #[cfg(windows)]
    fn env_expansion() {
        assert_eq!(expand_env("no vars"), "no vars");
        assert_eq!(expand_env("%__NOT_DEFINED__%"), "%__NOT_DEFINED__%");
        let p = expand_env("%USERPROFILE%");
        assert!(!p.starts_with('%'));
    }

    #[test]
    fn auth_file_shape() {
        let mut s = Session::default();
        s.access_token = "at".into();
        s.session_token = "st".into();
        s.expires = Value::String("2030-01-01T00:00:00Z".into());
        s.user.email = "a@b.c".into();
        s.user.id = "u1".into();
        s.account.id = "acc".into();
        s.account.plan_type = "plus".into();

        let a = build_auth(&s);
        assert_eq!(a.auth_mode, "chatgpt");
        assert_eq!(a.tokens.access_token, "at");
        assert_eq!(a.tokens.refresh_token, "st");
        assert_eq!(a.tokens.account_id, "acc");

        let parts: Vec<&str> = a.tokens.id_token.split('.').collect();
        assert_eq!(parts.len(), 3);
        assert_eq!(parts[2], "synthetic");
        assert!(a.last_refresh.ends_with('Z'));

        // 没有 sessionToken 时退回 placeholder
        let mut s2 = s.clone();
        s2.session_token.clear();
        assert_eq!(build_auth(&s2).tokens.refresh_token, "placeholder");
    }

    #[test]
    fn human_left_edge_cases() {
        assert_eq!(human_left(""), "");
        assert_eq!(human_left("not-a-date"), "");
        assert_eq!(human_left("2000-01-01T00:00:00Z"), "已过期");
        assert_eq!(fmt_local(""), "未知");
    }

    #[test]
    fn session_parses_null_fields() {
        let raw = r#"{"accessToken":"at","sessionToken":null,"expires":null,
                      "user":{"id":"u","email":"e"},"account":{"id":"a","planType":"pro"}}"#;
        let s: Session = serde_json::from_str(raw).expect("应当能解析");
        assert_eq!(s.access_token, "at");
        assert_eq!(s.expires_str(), "");
        assert_eq!(s.account.plan_type, "pro");
    }

    #[test]
    fn b64url_roundtrip_and_jwt_exp() {
        for sample in ["", "f", "fo", "foo", "foob", "你好，世界！"] {
            assert_eq!(
                b64url_decode(&b64url(sample.as_bytes())).as_deref(),
                Some(sample.as_bytes()),
            );
        }
        let bin: &[u8] = &[0u8, 255, 187];
        assert_eq!(
            b64url_decode(&b64url(bin)).as_deref(),
            Some(bin),
        );
        // 用自己的编码器拼一个 JWT，验证 exp 解析
        let payload = serde_json::json!({"exp": 1_800_000_000i64, "iat": 1_700_000_000i64, "sub": "u"});
        let jwt = format!("{}.{}.synthetic", b64url(br#"{"alg":"none"}"#), b64url_json(&payload));
        assert_eq!(jwt_payload_exp(&jwt), Some(1_800_000_000));
        assert_eq!(jwt_payload_iat(&jwt), Some(1_700_000_000));
        assert!(jwt_exp_iso(&jwt).is_some());
        assert_eq!(jwt_payload_exp("not-a-jwt"), None);
    }
}

