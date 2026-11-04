# codex-auth-helper · Rust 版

从本机已登录的 Chromium 系浏览器里读出 ChatGPT 会话，生成 Codex 需要的 `~/.codex/auth.json`。
纯本地处理，不经过任何服务器。

- 语言 / 平台：Rust，**Windows / macOS / Linux** 三平台一套代码（UI 用 eframe/egui）
- 形态：纯 GUI，单文件可执行（Windows 约 5.4 MB；mac/linux 为对应二进制）。
  Windows 以 GUI 子系统启动，不会弹出 cmd 黑窗口；拉起 `taskkill` / `reg` 等控制台
  子进程时也会复用隐藏控制台，避免闪窗。
- 核心依赖：`serde` / `serde_json` / `chrono` / `eframe`

> 本项目的思路与 Synthetic `id_token` 构造方案源自 [zhishile/codex-auth-helper](https://github.com/zhishile/codex-auth-helper)（Chrome 扩展版），在此向原作者致敬。本项目是其思路的 Rust 独立实现，界面沿用同一套自绘深色卡片风格。

## 它做了什么

1. 扫描本机已安装的 Chrome / Edge / Brave / Chromium，列出各自的用户配置（Profile）
2. 需要时优雅关闭浏览器，把 `Cookies` / `Preferences` / `Local State` 复制到临时目录
3. 用无头浏览器加载 `https://chatgpt.com/api/auth/session`，从 DOM 里取出会话 JSON
4. 构造一个 Synthetic `id_token`（`alg: none`，`cpa_synthetic: true`），拼出 Codex 的 `auth.json`
5. 备份旧配置后写入 `~/.codex/auth.json`，并把文件权限收紧到仅当前用户

## 用法与构建

```bash
cargo run --release      # 启动窗口界面
cargo build --release    # 产物：target/release/codex-auth-helper(.exe)
cargo test               # 13 个单测：base64url / JWT 解码 / JSON 提取 / 会话解析 / 字体回落 / 图标解码 / 窗口属性 / 运行态锁定
```

选浏览器 → 点「生成 auth.json」；失败时卡片里会给出针对当前原因的处理建议，改完点「重试」即可。
完成卡片里的「路径」为超链接样式，点击后定位该文件（资源管理器 / Finder / 文件管理器）。
处理中时转圈动画和「处理中…」显示在主按钮上，下方卡片只滚动显示日志；同时浏览器单选
胶囊与「备份旧配置」开关会变暗并锁定，等完成 / 失败后自动恢复，避免中途改选项。

- 启动时**自动选中系统默认浏览器**：Windows 读注册表 ProgId、macOS 读 LaunchServices、
  Linux 走 `xdg-settings`；读不到再回退到上次使用的。
- 界面里有**「备份旧配置」开关**（常驻），默认开启；关闭后旧 `auth.json` 会被直接覆盖，
  状态记忆在平台配置目录（`%APPDATA%` / `~/.config`）的 `CodexAuthHelper/settings.json`。

`CODEX_HOME` 环境变量可覆盖默认的 `~/.codex` 输出目录。

> `.cargo/config.toml` 里固定了 rsproxy 镜像源（官方源在国内经常断流，该文件不入库）。
> CI 上直接用官方源，无需处理。

### 图标、字体与版本信息

- `assets/app.ico`：6 档尺寸（16/24/32/48/64/128），绿色叠层图标。
  三平台的**窗口 / 任务栏图标**都在启动时从 ICO 里挑 64px 的 PNG 帧交给 egui；
  Windows 的 **exe 文件图标**再由 `app.rc` + `build.rs` 编进 `.res` 资源。
- `assets/NotoEmoji.ttf`：内置单色轮廓 emoji 字体（SIL OFL，许可证见
  `assets/OFL-NotoEmoji.txt`），与中文回落字体一起挂在默认 Proportional / Monospace
  字体链末尾。系统彩色 emoji 字体多是位图/COLR，egui 的 `ab_glyph` 读不出轮廓会
  渲染成空白，所以三平台统一用内置字体，文案里可以直接写 emoji。
- `app.rc`：ICON + VERSIONINFO（文件说明、版本、版权，资源 ID 均为 1）
- `build.rs`：自动在 `%RC%` 或 Windows SDK 里找 `rc.exe` 编成 `.res` 再交给链接器；
  找不到就跳过（不阻断构建，只在日志里留 warning）。mac/linux 上自动跳过。

### 平台差异与限制

- **浏览器发现**：三平台各自枚举安装路径与 User Data 目录（Windows `%LOCALAPPDATA%`、
  macOS `~/Library/Application Support`、Linux `~/.config`）。
- **关闭浏览器**：Windows 用 `taskkill`，mac/linux 用 `pkill`。unix 上浏览器不独占锁
  Cookies 文件，多数情况无需关闭即可直接读取。
- **窗口禁用最大化**：标题栏只保留最小化 / 关闭，Windows 上去掉 `WS_MAXIMIZEBOX` 后
  双击标题栏、`Win+↑`、任务栏右键的「最大化」也一并失效；macOS 禁用绿色 zoom 按钮。
  窗口仍可用边框手动调整大小。X11 下 winit 不提供该能力，最大化按钮可能仍在（Wayland 正常）。
- **权限收紧**：写完 `auth.json` 后 Windows 走 `icacls /inheritance:r`，mac/linux 直接 `chmod 600`。
- **macOS 注意**：无头浏览器解密 Cookie 需要 Keychain 允许（系统可能弹窗要求授权
  "chrome 要使用钥匙串"）；Linux 需要 gnome-keyring / kwallet 处于解锁状态。
- CI（GitHub Actions）在三个平台跑测试 + 构建，Release 会附上三平台的压缩包。

## 已知限制

- Codex 端报 **"Your access token could not be refreshed. Please log out and sign in again."**：
  本方案的 `refresh_token` 是 ChatGPT 的 sessionToken，不是真 OAuth refresh_token，所以 Codex
  只要尝试刷新就必然失败。而 Codex 只在两种情况下刷新：access_token 距过期不足 5 分钟、
  或后端请求返回 401 —— **后者才是常见根因：session 端点返回的是缓存的旧 access_token，
  它可能已被服务端轮换吊销，重新导出也还是同一个**（看窗口里的「有效期」没变就知道）。
  **处置：在所选浏览器里退出并重新登录 chatgpt.com（拿到新签发的 token），再重新导出。**
  工具会在导出的 token 签发超过 3 天时给出黄色警告。另外刷新失败后 Codex 会自行删除
  auth.json，属正常现象，重新导出即可。
- ChatGPT Free 账号能用 Codex 是限时福利，额度有限且随时可能收回；若重新登录后依旧报错，
  大概率是账号侧被收回权限，与工具无关。
- 本机实测 Edge 156 的 `--headless=new --dump-dom` 输出为空，Chrome 156 正常。若 Edge 读不到会话，请换 Chrome。
- 运行时需要短暂关闭所选浏览器（Windows 上 Cookie 库被独占锁；unix 一般不用关），完成后保持关闭，不会自动重开。

## 安全说明

- 全程本地读写，没有任何上传接口。
- 生成的 `auth.json` 权限收紧到仅当前用户（Windows `icacls /inheritance:r`，mac/linux `chmod 600`）。
- 旧配置写入前自动备份为 `auth.json.bak-YYYYMMDD-HHmmss`。
- `refresh_token` 用的是 ChatGPT 的 `sessionToken`，不是真正的 OAuth2 refresh_token。
  真正的 refresh_token 只能走 `auth.openai.com` 的完整验证流程。Codex 刷新失败时重新导出一份即可。
