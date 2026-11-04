//! Codex 登录助手 —— Rust 版
//!
//! 纯 GUI（eframe/egui），Windows / macOS / Linux 一套代码。

// Windows 下以 GUI 子系统启动：不会再额外弹出一个 cmd 黑窗口。
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

/// GUI 子系统进程自身没有控制台。它拉起 taskkill / reg / icacls 这类控制台程序时，
/// Windows 会给子进程新建一个可见黑窗口。这里先给自己挂一个隐藏控制台，
/// 子进程继承它就不会再弹窗（比逐个 Command 设置 CREATE_NO_WINDOW 更省心）。
#[cfg(windows)]
fn hide_child_consoles() {
    use std::ffi::c_void;

    #[link(name = "kernel32")]
    extern "system" {
        fn AllocConsole() -> i32;
        fn GetConsoleWindow() -> *mut c_void;
    }
    #[link(name = "user32")]
    extern "system" {
        fn ShowWindow(hwnd: *mut c_void, n_cmd_show: i32) -> i32;
    }

    const SW_HIDE: i32 = 0;
    unsafe {
        if AllocConsole() != 0 {
            let hwnd = GetConsoleWindow();
            if !hwnd.is_null() {
                ShowWindow(hwnd, SW_HIDE);
            }
        }
    }
}

/// 窗口属性。这里统一定义，方便单测校验（尤其是「禁用最大化」）。
fn viewport() -> egui::ViewportBuilder {
    let mut vp = egui::ViewportBuilder::default()
        .with_inner_size([580.0, 464.0])
        .with_min_inner_size([520.0, 430.0])
        // 禁用最大化：Windows 去掉 WS_MAXIMIZEBOX（最大化按钮置灰，双击标题栏、
        // Win+↑、任务栏菜单的「最大化」一并失效），macOS 禁用绿色 zoom 按钮。
        // 窗口仍可用边框手动调整大小；X11 下 winit 不支持该设置，见 README。
        .with_maximize_button(false)
        .with_maximized(false);
    if let Some(icon) = window_icon() {
        vp = vp.with_icon(icon);
    }
    vp
}

fn main() -> eframe::Result<()> {
    #[cfg(windows)]
    hide_child_consoles();

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
    fn maximize_is_disabled_but_window_is_still_resizable() {
        let vp = viewport();
        assert_eq!(vp.maximize_button, Some(false));
        assert_eq!(vp.maximized, Some(false));
        assert_ne!(vp.resizable, Some(false));
        assert!(vp.icon.is_some());
    }
}
