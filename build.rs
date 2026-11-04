//! 把图标和版本信息编成 .res 再交给链接器。
//!
//! 不用 windres / embed-resource：Windows SDK 自带 rc.exe，直接调它最省事，
//! 找不到就跳过（不阻断构建），只在编译日志里留个 warning。

use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=app.rc");
    println!("cargo:rerun-if-changed=assets/app.ico");

    // 只有 Windows 目标才需要 .res 资源，别在 macOS / Linux 的 CI 里刷 warning
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    let res = out_dir.join("app.res");

    let Some(rc) = find_rc() else {
        println!("cargo:warning=未找到 rc.exe（Windows SDK），跳过图标与版本资源");
        return;
    };

    let status = Command::new(&rc)
        .arg("/nologo")
        .arg("/fo")
        .arg(&res)
        .arg("app.rc")
        .status();

    match status {
        Ok(s) if s.success() && res.exists() => {
            println!("cargo:rustc-link-arg={}", res.display());
        }
        Ok(s) => println!("cargo:warning=rc.exe 退出码 {}，跳过图标资源", s),
        Err(e) => println!("cargo:warning=调用 rc.exe 失败: {}，跳过图标资源", e),
    }
}

/// 优先环境变量 RC，其次在 Windows Kits 里挑版本号最高的那个。
fn find_rc() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("RC") {
        let p = PathBuf::from(p);
        if p.exists() {
            return Some(p);
        }
    }

    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    let arch_dir = if arch == "x86" { "x86" } else { "x64" };

    let roots = [
        r"C:\Program Files (x86)\Windows Kits\10\bin",
        r"C:\Program Files\Windows Kits\10\bin",
    ];

    let mut best: Option<(Vec<u32>, PathBuf)> = None;
    for root in roots.iter() {
        let Ok(entries) = std::fs::read_dir(root) else {
            continue;
        };
        for e in entries.flatten() {
            let candidate = e.path().join(arch_dir).join("rc.exe");
            if !Path::new(&candidate).exists() {
                continue;
            }
            let ver = version_key(&e.file_name().to_string_lossy());
            if best.as_ref().map(|(v, _)| ver > *v).unwrap_or(true) {
                best = Some((ver, candidate));
            }
        }
    }
    best.map(|(_, p)| p)
}

fn version_key(name: &str) -> Vec<u32> {
    let mut v: Vec<u32> = name.split('.').map(|s| s.parse().unwrap_or(0)).collect();
    while v.len() < 4 {
        v.push(0);
    }
    v
}
