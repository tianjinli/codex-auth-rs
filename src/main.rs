//! Codex 登录助手 —— Rust 版
//!
//! 纯 GUI（eframe/egui），Windows / macOS / Linux 一套代码。

// Windows 下以 GUI 子系统启动，进程自身不挂控制台。所有子进程统一由
// auth::hidden() 带上 CREATE_NO_WINDOW，从源头避免黑窗口。
// 切勿改回 AllocConsole + ShowWindow(SW_HIDE)：那会先创建并显示控制台窗口、
// 再把它隐藏，这一帧就是启动瞬间「有个窗口开了又关」的闪烁来源。
#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

mod app;
mod auth;

use eframe::egui;

/// ICO 里的每一帧可以是 PNG（本项目 assets/app.ico 就是 6 帧 PNG）。
/// 挑尺寸最接近 `preferred` 的那帧，直接把 PNG 字节切片交回，不用引入 ico 解码库。
fn icon_png_from_ico(ico: &[u8], preferred: u32) -> Option<&[u8]> {
    if ico.len() < 6 {
        return None;
    }
    let count = u16::from_le_bytes([ico[4], ico[5]]) as usize;
    let mut best: Option<(u32, &[u8])> = None;

    for i in 0..count {
        let dir = 6 + 16 * i;
        if dir + 16 > ico.len() {
            break;
        }
        // ICO 目录项里宽/高为 0 表示 256
        let width = if ico[dir] == 0 { 256 } else { ico[dir] as u32 };
        let size = u32::from_le_bytes(ico[dir + 8..dir + 12].try_into().ok()?) as usize;
        let start = u32::from_le_bytes(ico[dir + 12..dir + 16].try_into().ok()?) as usize;
        let end = start.checked_add(size)?;
        if end > ico.len() {
            continue;
        }
        let png = &ico[start..end];
        if !png.starts_with(b"\x89PNG\r\n\x1a\n") {
            continue; // BMP/DIB 帧跳过
        }
        let score = width.abs_diff(preferred);
        if best.as_ref().map(|(s, _)| score < *s).unwrap_or(true) {
            best = Some((score, png));
        }
    }
    best.map(|(_, png)| png)
}

/// 窗口 / 任务栏图标。exe 文件图标由 build.rc → app.rc 里的 ICON 资源负责。
fn window_icon() -> Option<egui::IconData> {
    let png = icon_png_from_ico(include_bytes!("../assets/app.ico"), 64)?;
    eframe::icon_data::from_png_bytes(png).ok()
}

/// 窗口属性。这里统一定义，方便单测校验（固定尺寸 + 禁用最大化）。
fn viewport() -> egui::ViewportBuilder {
    let mut vp = egui::ViewportBuilder::default()
        .with_inner_size([580.0, 464.0])
        // 固定尺寸：Windows 去掉 WS_THICKFRAME，边框不能拖，双击标题栏 /
        // Win+↑ 也不再改变大小；macOS 同步禁用窗口缩放。
        .with_resizable(false)
        // 与 resizable 是两个独立开关，显式再关一次最大化按钮，双保险。
        .with_maximize_button(false)
        .with_maximized(false);
    if let Some(icon) = window_icon() {
        vp = vp.with_icon(icon);
    }
    vp
}

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: viewport(),
        ..Default::default()
    };
    eframe::run_native(
        "Codex 登录助手",
        options,
        Box::new(|cc| Ok(Box::new(app::CodexApp::new(cc)))),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ico_contains_png_frame() {
        let png = icon_png_from_ico(include_bytes!("../assets/app.ico"), 64);
        assert!(png.is_some());
        assert!(png.unwrap().starts_with(b"\x89PNG\r\n\x1a\n"));
    }

    #[test]
    fn icon_decodes() {
        assert!(window_icon().is_some());
    }

    #[test]
    fn window_is_fixed_size_and_not_maximizable() {
        let vp = viewport();
        assert_eq!(vp.resizable, Some(false));
        assert_eq!(vp.maximize_button, Some(false));
        assert_eq!(vp.maximized, Some(false));
        assert!(vp.icon.is_some());
    }
}
