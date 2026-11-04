//! eframe/egui 界面：深色卡片风，Windows / macOS / Linux 一套代码。
//!
//! 结构与旧 Win32 版一致：标题 → 浏览器选择组（GroupBox 风格）→ 备份开关 →
//! 主按钮 → 状态卡片（空闲说明 / 运行日志 / 完成结果 / 失败建议）。

use crate::auth::{self, Outcome, P_OK, P_STEP, P_WARN};
use eframe::egui;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::thread;

// 配色（与旧 Win32 版一致）
const COL_BG: u32 = 0x1C1D24;
const COL_CARD: u32 = 0x24252D;
const COL_CARD_EDGE: u32 = 0x33343E;
const COL_TEXT: u32 = 0xF2F3F7;
const COL_TEXT_DIM: u32 = 0x9B9DAB;
const COL_TEXT_MUTE: u32 = 0x6C6E7C;
const COL_ACCENT: u32 = 0x2EC27E;
const COL_ACCENT_HI: u32 = 0x3BD68F;
const COL_ON_ACCENT: u32 = 0x0B2018;
const COL_OK: u32 = 0x3FD07F;
const COL_WARN: u32 = 0xE0B341;
const COL_ERR: u32 = 0xF2685C;
const COL_LINK: u32 = 0x7FB3FF;
const COL_PILL: u32 = 0x2A2B34;
const COL_PILL_ON: u32 = 0x1E3A2E;
const COL_PILL_OFF_BTN: u32 = 0x3A3B45;

fn c32(v: u32) -> egui::Color32 {
    egui::Color32::from_rgb(
        ((v >> 16) & 0xFF) as u8,
        ((v >> 8) & 0xFF) as u8,
        (v & 0xFF) as u8,
    )
}

fn st(color: u32) -> egui::Stroke {
    egui::Stroke::new(1.0_f32, c32(color))
}

/// 运行中把控件配色压暗，直观表达「不可操作」。
fn dim_color(v: u32, k: f32) -> egui::Color32 {
    egui::Color32::from_rgb(
        (((v >> 16) & 0xFF) as f32 * k) as u8,
        (((v >> 8) & 0xFF) as f32 * k) as u8,
        ((v & 0xFF) as f32 * k) as u8,
    )
}

#[derive(Clone, Copy, PartialEq)]
enum State {
    Idle,
    Running,
    Done,
    Failed,
}

/// 运行中锁定浏览器单选与备份开关；完成后自动恢复。
fn controls_enabled(state: State) -> bool {
    !matches!(state, State::Running)
}

struct LogLine {
    kind: u8,
    text: String,
}

enum Msg {
    Log(u8, String),
    Done(Outcome),
    Fail(String),
}

pub struct CodexApp {
    browsers: Vec<auth::Browser>,
    selected: usize,
    state: State,
    logs: Vec<LogLine>,
    res: Option<Outcome>,
    err_msg: String,
    backup_on: bool,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
}

impl CodexApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        setup_fonts(&cc.egui_ctx);
        setup_visuals(&cc.egui_ctx);

        let browsers = auth::find_browsers();
        let mut selected = 0usize;
        if browsers.len() > 1 {
            // 优先选系统默认浏览器；没有则回退到上次使用的
            let mut pick = auth::default_browser_name();
            if pick.is_empty() {
                pick = auth::load_settings().last_browser;
            }
            for (i, b) in browsers.iter().enumerate() {
                if b.name.eq_ignore_ascii_case(&pick) {
                    selected = i;
                }
            }
        }

        let (tx, rx) = channel();
        CodexApp {
            browsers,
            selected,
            state: State::Idle,
            logs: Vec::new(),
            res: None,
            err_msg: String::new(),
            backup_on: auth::load_settings().backup,
            tx,
            rx,
        }
    }

    fn start(&mut self) {
        if self.state == State::Running {
            return;
        }
        self.state = State::Running;
        self.logs.clear();
        self.res = None;
        self.err_msg.clear();

        let name = self
            .browsers
            .get(self.selected)
            .map(|b| b.name.clone())
            .unwrap_or_default();
        let do_backup = self.backup_on;
        let tx = self.tx.clone();
        let tx_res = self.tx.clone();

        thread::spawn(move || {
            let prog = move |kind: u8, text: &str| {
                let _ = tx.send(Msg::Log(kind, text.to_string()));
            };
            match auth::run(&name, do_backup, &prog) {
                Ok(res) => {
                    let mut st = auth::load_settings();
                    st.last_browser = res.browser.clone();
                    auth::save_settings(&st);
                    let _ = tx_res.send(Msg::Done(res));
                }
                Err(e) => {
                    let _ = tx_res.send(Msg::Fail(e));
                }
            }
        });
    }

    fn open_path_location(&self, path: &str) {
        if path.is_empty() {
            return;
        }
        #[cfg(windows)]
        {
            // explorer /select,路径：打开资源管理器并选中该文件
            let _ = std::process::Command::new("explorer.exe")
                .arg(format!("/select,{}", path))
                .spawn();
        }
        #[cfg(target_os = "macos")]
        {
            // open -R：在 Finder 中显示该文件
            let _ = std::process::Command::new("open").args(["-R", path]).spawn();
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            let parent = std::path::Path::new(path)
                .parent()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| path.to_string());
            let _ = std::process::Command::new("xdg-open").arg(parent).spawn();
        }
    }
}

fn setup_visuals(ctx: &egui::Context) {
    let mut v = egui::Visuals::dark();
    v.panel_fill = c32(COL_BG);
    v.window_fill = c32(COL_BG);
    v.override_text_color = Some(c32(COL_TEXT));
    v.selection.bg_fill = c32(COL_ACCENT);
    v.selection.stroke = st(COL_ACCENT_HI);
    v.widgets.noninteractive.bg_fill = c32(COL_CARD);
    v.widgets.noninteractive.fg_stroke = st(COL_TEXT);
    v.widgets.inactive.fg_stroke = st(COL_TEXT_DIM);
    v.widgets.hovered.fg_stroke = st(COL_TEXT);
    ctx.set_visuals(v);
}

/// 字体回落链：默认 Latin → 中文 → 内置完整 emoji → egui 自带的两个 emoji 子集。
///
/// egui 自带字体不含 CJK；系统自带的彩色 emoji 字体（Windows Segoe UI Emoji 的
/// COLR、macOS Apple Color Emoji 的 sbix、Linux Noto Color Emoji 的 CBDT）在
/// ab_glyph 里读不出轮廓，容易渲染成空白。所以直接内置一份单色轮廓版
/// Noto Emoji（OFL，见 assets/OFL-NotoEmoji.txt），三平台表现一致。
fn setup_fonts(ctx: &egui::Context) {
    const EMOJI_TTF: &[u8] = include_bytes!("../assets/NotoEmoji.ttf");

    let cjk_candidates: &[&str] = &[
        // Windows
        r"C:\Windows\Fonts\msyh.ttc",
        r"C:\Windows\Fonts\msyh.ttf",
        r"C:\Windows\Fonts\simhei.ttf",
        // macOS
        "/System/Library/Fonts/PingFang.ttc",
        "/System/Library/Fonts/STHeiti Light.ttc",
        // Linux 常见发行版打包的 Noto CJK / 文泉驿
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/truetype/wqy/wqy-microhei.ttc",
    ];

    let mut fonts = egui::FontDefinitions::default();

    // 完整 emoji 字体：放在内置 emoji 子集之前，现代 emoji 也能画出来。
    fonts.font_data.insert(
        "emoji-full".to_owned(),
        egui::FontData::from_static(EMOJI_TTF).tweak(egui::FontTweak {
            scale: 1.0,
            ..Default::default()
        }),
    );

    // 中文字体：找得到才加，找不到不影响 emoji 与 Latin。
    let cjk = cjk_candidates
        .iter()
        .find_map(|p| std::fs::read(p).ok().map(|d| (*p, d)));
    if let Some((_, data)) = &cjk {
        fonts
            .font_data
            .insert("cjk".to_owned(), egui::FontData::from_owned(data.clone()));
    }

    if let Some(fam) = fonts.families.get_mut(&egui::FontFamily::Proportional) {
        // 先走默认 Latin 字形，缺的字依次回落到中文 / 完整 emoji
        let mut at = 1;
        if cjk.is_some() {
            fam.insert(at, "cjk".to_owned());
            at += 1;
        }
        fam.insert(at, "emoji-full".to_owned());
    }
    if let Some(fam) = fonts.families.get_mut(&egui::FontFamily::Monospace) {
        if cjk.is_some() {
            fam.push("cjk".to_owned());
        }
        let at = fam
            .iter()
            .position(|f| f == "NotoEmoji-Regular")
            .unwrap_or(fam.len());
        fam.insert(at, "emoji-full".to_owned());
    }

    ctx.set_fonts(fonts);
}

impl eframe::App for CodexApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // 收工作线程的消息
        let mut got = false;
        while let Ok(m) = self.rx.try_recv() {
            got = true;
            match m {
                Msg::Log(kind, text) => {
                    self.logs.push(LogLine { kind, text });
                    if self.logs.len() > 8 {
                        let cut = self.logs.len() - 8;
                        self.logs.drain(0..cut);
                    }
                }
                Msg::Done(r) => {
                    self.res = Some(r);
                    self.state = State::Done;
                }
                Msg::Fail(e) => {
                    self.err_msg = e;
                    self.state = State::Failed;
                }
            }
        }
        // 运行中保证界面持续刷新（转圈 + 日志轮询）
        if self.state == State::Running || got {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }

        egui::CentralPanel::default()
            .frame(
                egui::Frame::none()
                    .fill(c32(COL_BG))
                    .inner_margin(egui::Margin::symmetric(20.0, 14.0)),
            )
            .show(ctx, |ui| {
                self.draw_header(ui);
                ui.add_space(10.0);
                self.draw_browser_group(ui);
                ui.add_space(12.0);
                self.draw_primary_button(ui);
                ui.add_space(12.0);
                self.draw_state_card(ui);

                // 底部信息
                ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
                    ui.horizontal(|ui| {
                        ui.label(footer_text("v1.0.0"));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(footer_text("MIT · 本地处理"));
                        });
                    });
                });
            });
    }
}

fn footer_text(t: &str) -> egui::RichText {
    egui::RichText::new(t).size(10.0).color(c32(COL_TEXT_MUTE))
}

/// 运行态按钮：手动绘制，把 spinner 与文字作为一个整体水平居中。
/// 用 Sense::hover() 而不是 click，保证处理中无法再次触发。
fn running_button(
    ui: &mut egui::Ui,
    label: &str,
    fill: egui::Color32,
    txt_col: egui::Color32,
) -> egui::Response {
    let (rect, resp) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 40.0), egui::Sense::hover());
    if ui.is_rect_visible(rect) {
        ui.painter().rect(
            rect,
            egui::Rounding::same(10.0),
            fill,
            egui::Stroke::NONE,
        );
        let galley = egui::WidgetText::from(
            egui::RichText::new(label).size(15.0).strong().color(txt_col),
        )
        .into_galley(ui, None, f32::INFINITY, egui::TextStyle::Button);

        let spin = 18.0;
        let gap = 9.0;
        let total = spin + gap + galley.size().x;
        let cy = rect.center().y;
        let mut x = rect.center().x - total / 2.0;
        egui::Spinner::new().size(spin).color(txt_col).paint_at(
            ui,
            egui::Rect::from_center_size(egui::pos2(x + spin / 2.0, cy), egui::vec2(spin, spin)),
        );
        x += spin + gap;
        ui.painter().galley(
            egui::pos2(x, cy - galley.size().y / 2.0),
            galley,
            txt_col,
        );
    }
    resp
}

impl CodexApp {
    fn draw_header(&self, ui: &mut egui::Ui) {
        ui.label(
            egui::RichText::new("Codex 登录助手")
                .size(20.0)
                .strong()
                .color(c32(COL_TEXT)),
        );
        ui.label(
            egui::RichText::new("🔐 本地读取 ChatGPT 会话 · 生成 Codex 配置")
                .size(11.0)
                .color(c32(COL_TEXT_MUTE)),
        );
    }

    fn draw_browser_group(&mut self, ui: &mut egui::Ui) {
        let enabled = controls_enabled(self.state);
        egui::Frame::none()
            .fill(c32(COL_CARD))
            .stroke(st(COL_CARD_EDGE))
            .rounding(egui::Rounding::same(10.0))
            .inner_margin(egui::Margin::same(12.0))
            .show(ui, |ui| {
                // Frame 默认会缩到内容宽度；撑满后与按钮、状态卡片左右对齐
                ui.set_width(ui.available_width());
                ui.label(
                    egui::RichText::new("选择浏览器")
                        .size(11.0)
                        .color(c32(COL_TEXT_DIM)),
                );
                ui.add_space(4.0);

                if self.browsers.is_empty() {
                    ui.label(
                        egui::RichText::new("未找到 Chrome / Edge / Brave")
                            .size(12.0)
                            .color(c32(COL_ERR)),
                    );
                } else {
                    ui.horizontal_wrapped(|ui| {
                        for (i, b) in self.browsers.iter().enumerate() {
                            let sel = i == self.selected;
                            // 运行中整体压暗，并禁掉点击；标题与提示文字保持原样
                            let text_col = if sel { COL_ACCENT } else { COL_TEXT_DIM };
                            let bg_col = if sel { COL_PILL_ON } else { COL_PILL };
                            let edge_col = if sel { COL_ACCENT } else { COL_CARD_EDGE };
                            let btn = egui::Button::new(
                                egui::RichText::new(&b.name).size(12.0).color(if enabled {
                                    c32(text_col)
                                } else {
                                    dim_color(text_col, 0.55)
                                }),
                            )
                            .fill(if enabled {
                                c32(bg_col)
                            } else {
                                dim_color(bg_col, 0.72)
                            })
                            .stroke(if enabled {
                                st(edge_col)
                            } else {
                                egui::Stroke::new(1.0_f32, dim_color(edge_col, 0.6))
                            })
                            .rounding(egui::Rounding::same(7.0))
                            .min_size(egui::vec2(76.0, 28.0));
                            if ui.add_enabled(enabled, btn).clicked() {
                                self.selected = i;
                            }
                        }
                    });
                }

                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    let hint = if self.browsers.is_empty() {
                        "请先安装 Chrome 或 Edge 后重试"
                    } else {
                        "运行时会短暂关闭所选浏览器，完成后不会自动重开"
                    };
                    ui.label(egui::RichText::new(hint).size(10.5).color(c32(COL_TEXT_MUTE)));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let resp = ui
                            .add_enabled_ui(enabled, |ui| {
                                if !enabled {
                                    // 禁用淡出在深色主题下反而会提亮，这里显式压暗复选框配色
                                    let bg = dim_color(COL_PILL_OFF_BTN, 0.55);
                                    // 方框压暗、勾线保留可辨亮度，禁用时仍能看出开/关
                                    let fg =
                                        egui::Stroke::new(1.6_f32, dim_color(COL_TEXT, 0.72));
                                    let v = ui.visuals_mut();
                                    for w in [
                                        &mut v.widgets.inactive,
                                        &mut v.widgets.hovered,
                                        &mut v.widgets.active,
                                    ] {
                                        w.bg_fill = bg;
                                        w.weak_bg_fill = bg;
                                        w.bg_stroke = fg;
                                        w.fg_stroke = fg;
                                    }
                                }
                                ui.add(egui::Checkbox::new(
                                    &mut self.backup_on,
                                    egui::RichText::new("备份旧配置")
                                        .size(11.0)
                                        .color(c32(COL_TEXT_DIM)),
                                ))
                            })
                            .inner;
                        if resp.changed() {
                            let mut st = auth::load_settings();
                            st.backup = self.backup_on;
                            auth::save_settings(&st);
                        }
                    });
                });
            });
    }

    fn draw_primary_button(&mut self, ui: &mut egui::Ui) {
        let running = self.state == State::Running;
        let label = match self.state {
            State::Running => "处理中…",
            State::Done => "重新生成",
            State::Failed => "重试",
            State::Idle => "生成 auth.json",
        };
        let (fill, txt_col) = if running {
            (c32(COL_PILL_OFF_BTN), c32(COL_TEXT_DIM))
        } else {
            (c32(COL_ACCENT), c32(COL_ON_ACCENT))
        };
        // 运行态：动画和「处理中…」一起居中画在按钮上；日志框不再重复显示
        if running {
            running_button(ui, label, fill, txt_col);
            return;
        }
        // 其它状态放进 cross_align=Center 的子布局里，按钮文字才会水平居中
        // （egui 的 Button 是按当前 ui.layout() 对齐文字的，默认是靠左）。
        let resp = ui
            .vertical_centered(|ui| {
                let btn = egui::Button::new(
                    egui::RichText::new(label).size(15.0).strong().color(txt_col),
                )
                .fill(fill)
                .rounding(egui::Rounding::same(10.0))
                .min_size(egui::vec2(ui.available_width(), 40.0));
                ui.add(btn)
            })
            .inner;
        if resp.clicked() {
            self.start();
        }
    }

    fn draw_state_card(&mut self, ui: &mut egui::Ui) {
        egui::Frame::none()
            .fill(c32(COL_CARD))
            .stroke(st(COL_CARD_EDGE))
            .rounding(egui::Rounding::same(10.0))
            .inner_margin(egui::Margin::same(14.0))
            .show(ui, |ui| {
                // 与上面的「选择浏览器」框同宽
                ui.set_width(ui.available_width());
                match self.state {
                    State::Idle => {
                        let backup_line = if self.backup_on {
                            "· 旧文件自动备份，全程不上传任何数据"
                        } else {
                            "· 已关闭备份，旧 auth.json 会被直接覆盖"
                        };
                        for line in [
                            "· 需要先在浏览器里登录 chatgpt.com",
                            "· 生成的配置会写入 ~/.codex/auth.json",
                            backup_line,
                        ] {
                            ui.label(
                                egui::RichText::new(line)
                                    .size(12.0)
                                    .color(c32(COL_TEXT_MUTE)),
                            );
                        }
                    }
                    State::Running => {
                        // 日志框只放日志；转圈动画和「处理中…」已移到主按钮上
                        egui::ScrollArea::vertical()
                            .max_height(110.0)
                            .stick_to_bottom(true)
                            .show(ui, |ui| {
                                for l in &self.logs {
                                    let col = match l.kind {
                                        P_OK => COL_OK,
                                        P_WARN => COL_WARN,
                                        P_STEP => COL_TEXT,
                                        _ => COL_TEXT_DIM,
                                    };
                                    ui.label(
                                        egui::RichText::new(format!("· {}", l.text))
                                            .size(11.5)
                                            .color(c32(col)),
                                    );
                                }
                            });
                    }
                    State::Done => {
                        if let Some(r) = &self.res {
                            let left = if r.left.is_empty() { "—".to_string() } else { r.left.clone() };
                            self.kv(ui, "邮箱", &or_dash(&r.email), COL_TEXT);
                            self.kv(ui, "套餐", &r.plan, COL_TEXT);
                            self.kv(
                                ui,
                                "有效期",
                                &format!("{}（剩余 {}）", r.expires, left),
                                COL_TEXT_DIM,
                            );
                            ui.horizontal(|ui| {
                                ui.label(
                                    egui::RichText::new("路径")
                                        .size(11.0)
                                        .color(c32(COL_TEXT_MUTE)),
                                );
                                if ui
                                    .link(
                                        egui::RichText::new(&r.path)
                                            .size(12.0)
                                            .color(c32(COL_LINK)),
                                    )
                                    .on_hover_text("点击在文件管理器中定位")
                                    .clicked()
                                {
                                    self.open_path_location(&r.path);
                                }
                            });
                            let ok = if r.backup.is_empty() {
                                format!(
                                    "✓ 完成，新配置已写入 · 来源 {} / {}",
                                    r.browser, r.profile
                                )
                            } else {
                                format!(
                                    "✓ 完成，新配置已写入，旧配置已备份 · 来源 {} / {}",
                                    r.browser, r.profile
                                )
                            };
                            ui.label(
                                egui::RichText::new(ok)
                                    .size(11.0)
                                    .color(c32(COL_OK)),
                            );
                        }
                    }
                    State::Failed => {
                        ui.label(
                            egui::RichText::new("生成失败")
                                .size(13.0)
                                .strong()
                                .color(c32(COL_ERR)),
                        );
                        // err_msg 是「原因 + 编号建议」的多行文本
                        ui.label(
                            egui::RichText::new(&self.err_msg)
                                .size(11.5)
                                .color(c32(COL_TEXT_DIM)),
                        );
                    }
                }
            });
    }

    fn kv(&self, ui: &mut egui::Ui, k: &str, v: &str, col: u32) {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(k).size(11.0).color(c32(COL_TEXT_MUTE)));
            ui.label(egui::RichText::new(v).size(12.5).color(c32(col)));
        });
    }
}

fn or_dash(s: &str) -> &str {
    if s.is_empty() {
        "—"
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn controls_locked_only_while_running() {
        assert!(controls_enabled(State::Idle));
        assert!(controls_enabled(State::Done));
        assert!(controls_enabled(State::Failed));
        assert!(!controls_enabled(State::Running));
    }

    #[test]
    fn fonts_cover_cjk_and_full_emoji_range() {
        let ctx = egui::Context::default();
        setup_fonts(&ctx);
        let _ = ctx.run(egui::RawInput::default(), |_| {}); // 初始化字体图集
        let id = egui::FontId::new(14.0, egui::FontFamily::Proportional);
        ctx.fonts(|f| {
            assert!(f.has_glyphs(&id, "中"));
            // 这两个是 egui 内置 emoji 子集里没有、必须靠内置完整字体兜底的码位
            assert!(f.has_glyph(&id, '🔐'));
            assert!(f.has_glyph(&id, '🫠'));
        });
        let mono = egui::FontId::new(14.0, egui::FontFamily::Monospace);
        ctx.fonts(|f| assert!(f.has_glyph(&mono, '🚀')));
    }
}
