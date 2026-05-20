use anyhow::{anyhow, Context, Result};
use futures::StreamExt;
use tokio::sync::{mpsc, oneshot, RwLock};
use tokio::time::{timeout, Duration};

// std
use std::{borrow::Cow, collections::HashMap, io, sync::Arc};

// crossterm
use crossterm::{
    event::{Event, EventStream, KeyCode, KeyEvent, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};

// ratatui
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Paragraph, Row, Table, TableState},
    Frame, Terminal,
};

// crate
use crate::{
    format_u16, modbus::frame_bytes_from_info, modbus::RegCmd, parse_u16_str, AppState, Args,
    DisplayBase, FrameInfo, MainMode, MonitorStats,
};
const UI_TIMEOUT: Duration = Duration::from_secs(5);

/// 菜单屏幕状态
#[derive(Clone, Copy, PartialEq, Eq)]
enum MenuScreen {
    Main,        // 主菜单
    ProfilePick, // 选择配置（Server/Client 子菜单）
    ProfileSet,  // 配置管理设置
    ProfileEdit, // 编辑配置字段
    NamePrompt,  // 输入新增/克隆的配置名
}

/// 菜单选择结果，返回给 main.rs
#[derive(Clone, Debug)]
pub struct MenuSelection {
    pub main_mode: MainMode,
    pub profile_name: Option<String>,
    pub quit: bool,
}

pub struct Ui {
    base: DisplayBase,
    selected: usize,
    scroll: usize,
    edit_mode: bool,
    edit_is_label: bool,
    edit_is_profile: bool,
    edit_buf: String,
    status_msg: Option<String>,
    show_byte_panel: bool,

    // --- 静默监听 ---
    show_monitor: bool,
    /// 历史记录滚动偏移
    monitor_scroll: usize,
    /// true=焦点在历史面板, false=焦点在统计面板
    monitor_focus_history: bool,

    // --- 菜单相关字段 ---
    /// 当前菜单屏幕：Main / ProfilePick / ProfileSet
    menu_screen: MenuScreen,
    /// ProfilePick/ProfileSet 以及主菜单中的选中项索引
    menu_list_idx: usize,
    /// 加载到的所有配置名
    profiles: Vec<String>,
    /// 当前选中的配置
    selected_profile: Option<String>,
    /// 待选模式（从主菜单进入子菜单时暂存）
    pending_mode: Option<MainMode>,
    /// 当前默认配置名
    default_profile: Option<String>,
    /// 主菜单渲染用的 Logo ASCII ART 行
    logo_lines: Vec<String>,

    // --- 配置编辑相关字段 ---
    /// 正在编辑的配置名
    edit_profile_name: Option<String>,
    /// 正在编辑的配置参数副本
    edit_args: Option<Args>,
    /// 编辑字段缓冲
    field_edit_buf: String,
    /// 是否正在编辑字段值（true=编辑模式，false=导航模式）
    field_edit_mode: bool,

    // --- 新增/克隆配置相关字段 ---
    /// 新增/克隆时的名称输入缓冲
    name_prompt_buf: String,
    /// true=克隆选中配置, false=新建空配置
    name_prompt_is_clone: bool,
    /// 克隆时暂存的原配置 Args
    name_prompt_clone_args: Option<Args>,
}

impl Ui {
    fn new(base: DisplayBase, profiles: Vec<String>) -> Self {
        let logo_raw = include_str!("logo.txt");
        let mut logo_lines: Vec<String> = logo_raw.lines().map(|l| l.to_string()).collect();
        logo_lines.push(format!("                   v{}", env!("CARGO_PKG_VERSION")));
        let default = profiles.first().cloned();
        Self {
            base,
            selected: 0,
            scroll: 0,
            edit_mode: false,
            edit_is_label: false,
            edit_is_profile: false,
            edit_buf: String::new(),
            status_msg: None,
            show_byte_panel: true,
            show_monitor: false,
            monitor_scroll: 0,
            monitor_focus_history: true,

            menu_screen: MenuScreen::Main,
            menu_list_idx: 0,
            profiles,
            selected_profile: default.clone(),
            pending_mode: None,
            default_profile: default,
            logo_lines,

            // 配置编辑
            edit_profile_name: None,
            edit_args: None,
            field_edit_buf: String::new(),
            field_edit_mode: false,

            // 新增/克隆
            name_prompt_buf: String::new(),
            name_prompt_is_clone: false,
            name_prompt_clone_args: None,
        }
    }
}

fn edit_accepts_char(current: &str, ch: char, base: DisplayBase) -> bool {
    if ch.is_ascii_whitespace() {
        return false;
    }
    if ch == 'x' || ch == 'X' || ch == 'b' || ch == 'B' {
        return current == "0" || current.eq_ignore_ascii_case("0");
    }
    if ch == '0' && current.is_empty() {
        return true;
    }
    match base {
        DisplayBase::Dec => ch.is_ascii_digit(),
        DisplayBase::Hex => ch.is_ascii_hexdigit(),
        DisplayBase::Bin => ch == '0' || ch == '1',
    }
}

fn set_status(ui: &mut Ui, msg: impl Into<String>) {
    ui.status_msg = Some(msg.into());
}

/// 加载配置列表，返回所有配置名
pub fn load_profile_list(config_path: &str) -> Vec<String> {
    let config_str = std::fs::read_to_string(config_path).unwrap_or_default();
    let configs: HashMap<String, Args> = toml::from_str(&config_str).unwrap_or_default();
    let mut names: Vec<String> = configs.into_keys().collect();
    names.sort();
    names
}

/// 根据配置名加载配置，返回 (配置名, Args)
pub fn load_profile_args(config_path: &str, name: &str) -> Option<(String, Args)> {
    let config_str = std::fs::read_to_string(config_path).ok()?;
    let mut configs: HashMap<String, Args> = toml::from_str(&config_str).ok()?;
    configs.remove(name).map(|a| (name.to_string(), a))
}

/// 保存默认配置标记（写入到配置文件的 default 字段）
fn save_default_profile(config_path: &str, name: &str) -> Result<()> {
    let config_str = std::fs::read_to_string(config_path).unwrap_or_default();
    let mut configs: HashMap<String, Args> = toml::from_str(&config_str).unwrap_or_default();
    // 把默认配置名存为 key "default" 的特殊配置，只存一个 name 字段
    let mut default_args = Args::default();
    default_args.main_mode = name.to_string(); // 借用 main_mode 字段存储默认配置名
    configs.insert("__default__".to_string(), default_args);
    let s = toml::to_string_pretty(&configs)?;
    std::fs::write(config_path, s)?;
    Ok(())
}

/// 读取默认配置名
fn load_default_profile(config_path: &str) -> Option<String> {
    let config_str = std::fs::read_to_string(config_path).ok()?;
    let configs: HashMap<String, Args> = toml::from_str(&config_str).ok()?;
    configs.get("__default__").map(|a| a.main_mode.clone())
}

// ─────────────────────────────────────────
// 菜单渲染与事件处理
// ─────────────────────────────────────────

/// 主菜单：垂直排列，每个模式作为一个独立菜单项
fn render_main_menu(f: &mut Frame<'_>, ui: &Ui) {
    let area = f.area();
    let vert = Layout::vertical([
        Constraint::Length(7), // Logo + version
        Constraint::Min(12),   // 菜单项
        Constraint::Length(3), // 底部信息栏
    ])
    .split(area);

    // --- Logo 区 ---
    let logo_style = Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD);
    let logo_text: Vec<Line> = ui
        .logo_lines
        .iter()
        .map(|l| Line::from(Span::styled(l.clone(), logo_style)))
        .collect();
    f.render_widget(
        Paragraph::new(logo_text).alignment(ratatui::layout::Alignment::Center),
        vert[0],
    );

    // --- 主菜单区：垂直排列 ---
    let menu_items: [(&str, MainMode); 5] = [
        ("main_menu.tcp_server", MainMode::TcpServer),
        ("main_menu.tcp_client", MainMode::TcpClient),
        ("main_menu.rtu_server", MainMode::RTUServer),
        ("main_menu.rtu_client", MainMode::RTUClient),
        ("main_menu.monitor", MainMode::Monitor),
    ];

    let block = Block::default()
        .borders(Borders::ALL)
        .title(t!("main_menu.title"))
        .border_style(Style::default().fg(Color::Cyan));

    let inner = block.inner(vert[1]);
    f.render_widget(block, vert[1]);

    let mut lines: Vec<Line> = Vec::new();

    // 渲染 4 个模式菜单项
    for (i, &(key, _mode)) in menu_items.iter().enumerate() {
        let is_selected = i == ui.menu_list_idx;
        let item_style = if is_selected {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        let prefix = if is_selected { " ▸ " } else { "   " };
        let label = t!(key);
        lines.push(Line::from(Span::styled(
            format!("{}[ {} ]", prefix, label),
            item_style,
        )));
        lines.push(Line::from(Span::raw(""))); // spacer
    }

    // 渲染 Profile Settings 项
    let i = menu_items.len();
    let is_selected = i == ui.menu_list_idx;
    let p_style = if is_selected {
        Style::default()
            .fg(Color::Black)
            .bg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    };
    let prefix = if is_selected { " ▸ " } else { "   " };
    lines.push(Line::from(Span::styled(
        format!("{}[ {} ]", prefix, t!("main_menu.profile_settings")),
        p_style,
    )));

    let paragraph = Paragraph::new(lines).alignment(ratatui::layout::Alignment::Left);
    f.render_widget(paragraph, inner);

    // --- 底部信息栏 ---
    let default_name = ui
        .default_profile
        .as_deref()
        .map(std::borrow::Cow::Borrowed)
        .unwrap_or_else(|| t!("main_menu.none"));
    let selected = ui
        .selected_profile
        .as_deref()
        .map(std::borrow::Cow::Borrowed)
        .unwrap_or_else(|| t!("main_menu.none"));
    let status = t!(
        "main_menu.info_bar",
        default = default_name,
        selected = selected
    );
    f.render_widget(
        Paragraph::new(status)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(t!("main_menu.info")),
            )
            .style(Style::default().fg(Color::DarkGray)),
        vert[2],
    );
}

/// 配置选择子菜单（ProfilePick）：预览并选择配置
fn render_profile_pick(f: &mut Frame<'_>, ui: &Ui, config_path: &str) {
    let area = f.area();
    let vert = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(10),
        Constraint::Length(3),
    ])
    .split(area);

    // 标题
    let mode_name = match ui.pending_mode {
        Some(MainMode::TcpServer) => "TCP Server",
        Some(MainMode::TcpClient) => "TCP Client",
        Some(MainMode::RTUServer) => "RTU Server",
        Some(MainMode::RTUClient) => "RTU Client",
        Some(MainMode::Monitor) | None => "?",
    };
    let title = t!("profile_pick.title", mode = mode_name);
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            title.as_ref(),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )))
        .alignment(ratatui::layout::Alignment::Center),
        vert[0],
    );

    // 主区域：左列表 + 右预览
    let main =
        Layout::horizontal([Constraint::Percentage(40), Constraint::Percentage(60)]).split(vert[1]);

    // 左：配置列表
    let mut items: Vec<Line> = Vec::new();
    for (i, name) in ui.profiles.iter().enumerate() {
        let is_sel = i == ui.menu_list_idx;
        let is_default = Some(name.as_str()) == ui.default_profile.as_deref();
        let prefix = if is_default { "★ " } else { "  " };
        let line = if is_sel {
            Line::from(Span::styled(
                format!("{prefix}{name}"),
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ))
        } else {
            Line::from(Span::styled(format!("{prefix}{name}"), Style::default()))
        };
        items.push(line);
    }
    if ui.profiles.is_empty() {
        items.push(Line::from(Span::styled(
            t!("profile_pick.empty_list"),
            Style::default().fg(Color::DarkGray),
        )));
    }

    let list_block = Block::default()
        .borders(Borders::ALL)
        .title(t!("profile_pick.list_title"))
        .border_style(Style::default().fg(Color::Cyan));
    f.render_widget(Paragraph::new(items).block(list_block), main[0]);

    // 右：预览所选配置
    let preview_text = if ui.menu_list_idx < ui.profiles.len() {
        let name = &ui.profiles[ui.menu_list_idx];
        if let Some((_, args)) = load_profile_args(config_path, name) {
            format!(
                "{}\n\
                 ─────────────────\n\
                 {}\n\
                 {}\n\
                 {}\n\
                 {}\n\
                 {}\n\
                 {}",
                t!("profile_pick.preview_name", name = name),
                t!("profile_pick.preview_mode", mode = &args.main_mode),
                t!("profile_pick.preview_port", port = args.tcp_port),
                t!("profile_pick.preview_unit", unit = args.unit),
                t!("profile_pick.preview_count", count = args.holding_count),
                t!("profile_pick.preview_device", device = &args.device),
                t!("profile_pick.preview_baud", baud = args.baudrate),
            )
        } else {
            format!(
                "{}\n{}",
                t!("profile_pick.preview_name", name = name),
                t!("profile_pick.load_fail")
            )
        }
    } else {
        t!("profile_pick.select_hint").to_string()
    };
    let preview_block = Block::default()
        .borders(Borders::ALL)
        .title(t!("profile_pick.preview_title"))
        .border_style(Style::default().fg(Color::Green));
    f.render_widget(Paragraph::new(preview_text).block(preview_block), main[1]);

    // 底部帮助
    let help = t!("profile_pick.help");
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            help,
            Style::default().fg(Color::DarkGray),
        )))
        .alignment(ratatui::layout::Alignment::Center),
        vert[2],
    );
}

fn render_profile_settings(f: &mut Frame<'_>, ui: &Ui, _config_path: &str) {
    let area = f.area();
    let vert = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(10),
        Constraint::Length(3),
    ])
    .split(area);

    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            t!("profile_settings.title"),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )))
        .alignment(ratatui::layout::Alignment::Center),
        vert[0],
    );

    // 主体部分
    let main =
        Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)]).split(vert[1]);

    // 左：配置列表，可设置为默认
    let mut items: Vec<Line> = Vec::new();
    for (i, name) in ui.profiles.iter().enumerate() {
        let is_default = Some(name.as_str()) == ui.default_profile.as_deref();
        let icon = if is_default { "●" } else { "○" };
        let line = if i == ui.menu_list_idx {
            Line::from(Span::styled(
                format!(" {icon} {name}"),
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ))
        } else if is_default {
            Line::from(Span::styled(
                format!(" {icon} {name}"),
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ))
        } else {
            Line::from(Span::styled(format!(" {icon} {name}"), Style::default()))
        };
        items.push(line);
    }
    if ui.profiles.is_empty() {
        items.push(Line::from(Span::styled(
            t!("profile_settings.empty_list"),
            Style::default().fg(Color::DarkGray),
        )));
    }

    let list_block = Block::default()
        .borders(Borders::ALL)
        .title(t!("profile_settings.list_title"))
        .border_style(Style::default().fg(Color::Cyan));
    f.render_widget(Paragraph::new(items).block(list_block), main[0]);

    // 右：当前默认配置信息 + 操作说明
    let default_name = ui
        .default_profile
        .as_deref()
        .map(std::borrow::Cow::Borrowed)
        .unwrap_or_else(|| t!("profile_settings.info_default"));
    let info = t!("profile_settings.info_text", name = default_name);
    let info_block = Block::default()
        .borders(Borders::ALL)
        .title(t!("profile_settings.info_title"))
        .border_style(Style::default().fg(Color::Green));
    f.render_widget(Paragraph::new(info).block(info_block), main[1]);

    // 底部
    let help = t!("profile_settings.help");
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            help,
            Style::default().fg(Color::DarkGray),
        )))
        .alignment(ratatui::layout::Alignment::Center),
        vert[2],
    );
}

/// 处理主菜单的按键事件（垂直导航）
fn handle_main_menu_key(ui: &mut Ui, code: KeyCode) -> Option<MenuSelection> {
    const ITEM_COUNT: usize = 6; // TCP Server(0), TCP Client(1), RTU Server(2), RTU Client(3), Monitor(4), Profile Settings(5)

    match code {
        KeyCode::Up => {
            ui.menu_list_idx = ui.menu_list_idx.saturating_sub(1);
        }
        KeyCode::Down => {
            if ui.menu_list_idx + 1 < ITEM_COUNT {
                ui.menu_list_idx += 1;
            }
        }
        KeyCode::Enter => match ui.menu_list_idx {
            0 => {
                ui.pending_mode = Some(MainMode::TcpServer);
                ui.menu_screen = MenuScreen::ProfilePick;
                ui.menu_list_idx = 0;
            }
            1 => {
                ui.pending_mode = Some(MainMode::TcpClient);
                ui.menu_screen = MenuScreen::ProfilePick;
                ui.menu_list_idx = 0;
            }
            2 => {
                ui.pending_mode = Some(MainMode::RTUServer);
                ui.menu_screen = MenuScreen::ProfilePick;
                ui.menu_list_idx = 0;
            }
            3 => {
                ui.pending_mode = Some(MainMode::RTUClient);
                ui.menu_screen = MenuScreen::ProfilePick;
                ui.menu_list_idx = 0;
            }
            4 => {
                // Monitor 模式直接返回 Selection（不需要配置）
                return Some(MenuSelection {
                    main_mode: MainMode::Monitor,
                    profile_name: None,
                    quit: false,
                });
            }
            5 => {
                // Profile Settings
                ui.menu_screen = MenuScreen::ProfileSet;
                ui.menu_list_idx = 0;
            }
            _ => {}
        },
        KeyCode::Char('q') => {
            // 退出程序
            return Some(MenuSelection {
                main_mode: MainMode::TcpClient,
                profile_name: None,
                quit: true,
            });
        }
        _ => {}
    }
    None
}

/// 处理配置选择子菜单的按键事件
fn handle_profile_pick_key(
    ui: &mut Ui,
    code: KeyCode,
    _config_path: &str,
) -> Option<MenuSelection> {
    match code {
        KeyCode::Up => {
            if !ui.profiles.is_empty() {
                ui.menu_list_idx = ui.menu_list_idx.saturating_sub(1);
            }
        }
        KeyCode::Down => {
            if ui.menu_list_idx + 1 < ui.profiles.len() {
                ui.menu_list_idx += 1;
            }
        }
        KeyCode::Enter => {
            let mode = ui.pending_mode.unwrap_or(MainMode::TcpClient);
            let profile = if ui.menu_list_idx < ui.profiles.len() {
                Some(ui.profiles[ui.menu_list_idx].clone())
            } else {
                None
            };
            ui.selected_profile = profile.clone();
            return Some(MenuSelection {
                main_mode: mode,
                profile_name: profile,
                quit: false,
            });
        }
        KeyCode::Esc => {
            ui.menu_screen = MenuScreen::Main;
        }
        KeyCode::Char('q') => {
            return Some(MenuSelection {
                main_mode: MainMode::TcpClient,
                profile_name: None,
                quit: true,
            });
        }
        _ => {}
    }
    None
}

/// 处理配置管理设置的按键事件
fn handle_profile_set_key(ui: &mut Ui, code: KeyCode, config_path: &str) -> Option<MenuSelection> {
    match code {
        KeyCode::Up => {
            if !ui.profiles.is_empty() {
                ui.menu_list_idx = ui.menu_list_idx.saturating_sub(1);
            }
        }
        KeyCode::Down => {
            if ui.menu_list_idx + 1 < ui.profiles.len() {
                ui.menu_list_idx += 1;
            }
        }
        KeyCode::Enter => {
            if ui.menu_list_idx < ui.profiles.len() {
                let name = ui.profiles[ui.menu_list_idx].clone();
                if save_default_profile(config_path, &name).is_ok() {
                    ui.default_profile = Some(name.clone());
                    ui.selected_profile = Some(name);
                    set_status(
                        ui,
                        t!(
                            "profile_settings.set_success",
                            name = ui.default_profile.as_deref().unwrap_or("")
                        ),
                    );
                } else {
                    set_status(ui, t!("profile_settings.set_fail"));
                }
            }
        }
        KeyCode::Char('e') => {
            // 编辑选中配置
            if ui.menu_list_idx < ui.profiles.len() {
                let name = ui.profiles[ui.menu_list_idx].clone();
                if let Some((_, args)) = load_profile_args(config_path, &name) {
                    ui.edit_profile_name = Some(name.clone());
                    ui.edit_args = Some(args);
                    ui.menu_list_idx = 0;
                    ui.field_edit_mode = false;
                    ui.field_edit_buf.clear();
                    ui.menu_screen = MenuScreen::ProfileEdit;
                } else {
                    set_status(ui, t!("profile_settings.load_fail"));
                }
            }
        }
        KeyCode::Char('a') => {
            // 新增配置
            ui.name_prompt_buf.clear();
            ui.name_prompt_is_clone = false;
            ui.name_prompt_clone_args = None;
            ui.menu_screen = MenuScreen::NamePrompt;
        }
        KeyCode::Char('c') => {
            // 克隆选中配置
            if ui.menu_list_idx < ui.profiles.len() {
                let name = ui.profiles[ui.menu_list_idx].clone();
                if let Some((_, args)) = load_profile_args(config_path, &name) {
                    ui.name_prompt_buf.clear();
                    ui.name_prompt_is_clone = true;
                    ui.name_prompt_clone_args = Some(args);
                    ui.menu_screen = MenuScreen::NamePrompt;
                } else {
                    set_status(ui, t!("profile_settings.load_fail"));
                }
            }
        }
        KeyCode::Esc => {
            ui.menu_screen = MenuScreen::Main;
        }
        KeyCode::Char('q') => {
            return Some(MenuSelection {
                main_mode: MainMode::TcpClient,
                profile_name: None,
                quit: true,
            });
        }
        _ => {}
    }
    None
}

// ─────────────────────────────────────────
// 配置字段编辑：字段定义、渲染、按键处理
// ─────────────────────────────────────────

/// 可编辑的配置字段描述
struct ProfileField {
    /// 字段名（i18n key 后缀）
    name_key: &'static str,
    /// 从 Args 中提取当前值的显示字符串
    display: Box<dyn Fn(&Args) -> String>,
    /// 将字符串解析为新值并设置到 Args 中
    apply: Box<dyn Fn(&mut Args, &str) -> Result<(), String>>,
}

/// 返回所有可编辑字段的列表
fn profile_fields() -> Vec<ProfileField> {
    fn main_mode_display(a: &Args) -> String {
        a.main_mode.clone()
    }
    fn main_mode_apply(a: &mut Args, v: &str) -> Result<(), String> {
        let v = v.trim().to_lowercase();
        match v.as_str() {
            "tcp-server" | "ts" => a.main_mode = "tcp-server".into(),
            "tcp-client" | "tc" => a.main_mode = "tcp-client".into(),
            "rtu-server" | "rs" => a.main_mode = "rtu-server".into(),
            "rtu-client" | "rc" => a.main_mode = "rtu-client".into(),
            _ => return Err(format!("无效模式: {v} (ts/tc/rs/rc)")),
        }
        Ok(())
    }
    fn num_display<T: ToString + 'static>(f: fn(&Args) -> T) -> Box<dyn Fn(&Args) -> String> {
        Box::new(move |a| f(a).to_string())
    }
    fn num_apply<T: std::str::FromStr + 'static>(
        set: fn(&mut Args, T),
        name: &'static str,
    ) -> Box<dyn Fn(&mut Args, &str) -> Result<(), String>> {
        let name = name;
        Box::new(move |a, v| {
            let v = v.trim();
            v.parse::<T>()
                .map(|val| set(a, val))
                .map_err(|_| format!("{name} 必须是有效数字"))
        })
    }
    fn str_apply(set: fn(&mut Args, String)) -> Box<dyn Fn(&mut Args, &str) -> Result<(), String>> {
        Box::new(move |a, v| {
            set(a, v.to_string());
            Ok(())
        })
    }

    vec![
        ProfileField {
            name_key: "main_mode",
            display: Box::new(main_mode_display),
            apply: Box::new(main_mode_apply),
        },
        ProfileField {
            name_key: "tcp_port",
            display: num_display(|a: &Args| a.tcp_port),
            apply: num_apply(|a: &mut Args, v| a.tcp_port = v, "TCP端口"),
        },
        ProfileField {
            name_key: "unit",
            display: num_display(|a: &Args| a.unit),
            apply: num_apply(|a: &mut Args, v| a.unit = v, "从站地址"),
        },
        ProfileField {
            name_key: "holding_count",
            display: num_display(|a: &Args| a.holding_count),
            apply: num_apply(|a: &mut Args, v| a.holding_count = v, "寄存器数"),
        },
        ProfileField {
            name_key: "client_tick_ms",
            display: num_display(|a: &Args| a.client_tick_ms),
            apply: num_apply(|a: &mut Args, v| a.client_tick_ms = v, "轮询间隔"),
        },
        ProfileField {
            name_key: "device",
            display: Box::new(|a: &Args| a.device.clone()),
            apply: str_apply(|a: &mut Args, v| a.device = v),
        },
        ProfileField {
            name_key: "baudrate",
            display: num_display(|a: &Args| a.baudrate),
            apply: num_apply(|a: &mut Args, v| a.baudrate = v, "波特率"),
        },
        ProfileField {
            name_key: "parity",
            display: Box::new(|a: &Args| a.parity.clone()),
            apply: Box::new(|a: &mut Args, v: &str| {
                let v = v.trim().to_lowercase();
                match v.as_str() {
                    "n" | "none" => a.parity = "n".into(),
                    "e" | "even" => a.parity = "e".into(),
                    "o" | "odd" => a.parity = "o".into(),
                    _ => return Err(format!("无效校验位: {v} (n/e/o)")),
                }
                Ok(())
            }),
        },
        ProfileField {
            name_key: "flow",
            display: Box::new(|a: &Args| a.flow.clone()),
            apply: Box::new(|a: &mut Args, v: &str| {
                let v = v.trim().to_lowercase();
                match v.as_str() {
                    "none" => a.flow = "none".into(),
                    "hardware" | "hard" => a.flow = "hardware".into(),
                    "software" | "soft" => a.flow = "software".into(),
                    _ => return Err(format!("无效流控: {v} (none/hard/soft)")),
                }
                Ok(())
            }),
        },
        ProfileField {
            name_key: "databits",
            display: num_display(|a: &Args| a.databits),
            apply: Box::new(|a: &mut Args, v: &str| {
                let val: u8 = v
                    .trim()
                    .parse()
                    .map_err(|_| "数据位必须是数字".to_string())?;
                if ![5, 6, 7, 8].contains(&val) {
                    return Err("数据位必须是 5/6/7/8".to_string());
                }
                a.databits = val;
                Ok(())
            }),
        },
        ProfileField {
            name_key: "stopbits",
            display: num_display(|a: &Args| a.stopbits),
            apply: Box::new(|a: &mut Args, v: &str| {
                let val: u8 = v
                    .trim()
                    .parse()
                    .map_err(|_| "停止位必须是数字".to_string())?;
                if val != 1 && val != 2 {
                    return Err("停止位必须是 1 或 2".to_string());
                }
                a.stopbits = val;
                Ok(())
            }),
        },
    ]
}

/// 保存编辑后的配置到配置文件
fn save_edited_profile(config_path: &str, profile_name: &str, args: &Args) -> Result<()> {
    let config_str = std::fs::read_to_string(config_path).unwrap_or_default();
    let mut configs: HashMap<String, Args> = toml::from_str(&config_str).unwrap_or_default();
    configs.insert(profile_name.to_string(), args.clone());
    let s = toml::to_string_pretty(&configs)?;
    std::fs::write(config_path, s)?;
    Ok(())
}

/// 渲染配置编辑界面
fn render_profile_edit(f: &mut Frame<'_>, ui: &Ui, _config_path: &str) {
    let area = f.area();
    let vert = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(10),
        Constraint::Length(3),
    ])
    .split(area);

    let profile_name = ui.edit_profile_name.as_deref().unwrap_or("?");
    let title = t!("profile_edit.title", name = profile_name);
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            title.as_ref(),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )))
        .alignment(ratatui::layout::Alignment::Center),
        vert[0],
    );

    let fields = profile_fields();
    let main =
        Layout::horizontal([Constraint::Percentage(55), Constraint::Percentage(45)]).split(vert[1]);

    // 左：字段列表
    let mut lines: Vec<Line> = Vec::new();
    for (i, field) in fields.iter().enumerate() {
        let is_selected = i == ui.menu_list_idx;
        let args = ui.edit_args.as_ref().unwrap();
        let val = (field.display)(args);
        let label_key = format!("profile_edit.{}", field.name_key);
        let label = t!(&label_key);

        let line = if is_selected {
            Line::from(vec![
                Span::styled(" ▸ ", Style::default().fg(Color::Yellow)),
                Span::styled(
                    format!("{}: ", label),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::styled(val, Style::default().fg(Color::Black).bg(Color::Cyan)),
            ])
        } else {
            Line::from(vec![
                Span::raw("   "),
                Span::styled(format!("{}: ", label), Style::default()),
                Span::styled(val, Style::default().fg(Color::Green)),
            ])
        };
        lines.push(line);
    }

    let list_block = Block::default()
        .borders(Borders::ALL)
        .title(t!("profile_edit.fields_title"))
        .border_style(Style::default().fg(Color::Cyan));
    f.render_widget(Paragraph::new(lines).block(list_block), main[0]);

    // 右：编辑面板或提示
    let right_content = if ui.field_edit_mode {
        // 编辑模式下显示输入框
        let field = &fields[ui.menu_list_idx];
        let label_key = format!("profile_edit.{}", field.name_key);
        let label = t!(&label_key);
        let mut edit_lines = vec![
            Line::from(Span::styled(
                t!("profile_edit.editing", label = label.as_ref()),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::raw("")),
            Line::from(Span::styled(
                format!("> {}", ui.field_edit_buf),
                Style::default().fg(Color::Cyan),
            )),
            Line::from(Span::raw("")),
        ];
        // 如有校验提示，添加提示信息
        let hint = match field.name_key {
            "main_mode" => "ts/tc/rs/rc 或 tcp-client/tcp-server/rtu-client/rtu-server",
            "parity" => "n(even) / e(ven) / o(dd)",
            "flow" => "none / hard(ware) / soft(ware)",
            "databits" => "5 / 6 / 7 / 8",
            "stopbits" => "1 / 2",
            _ => "",
        };
        if !hint.is_empty() {
            edit_lines.push(Line::from(Span::styled(
                hint,
                Style::default().fg(Color::DarkGray),
            )));
            edit_lines.push(Line::from(Span::raw("")));
        }
        edit_lines.push(Line::from(Span::styled(
            t!("profile_edit.edit_help"),
            Style::default().fg(Color::DarkGray),
        )));
        Paragraph::new(edit_lines)
    } else {
        // 导航模式显示提示
        let help_text = if ui.edit_args.is_some() {
            t!("profile_edit.nav_help")
        } else {
            Cow::from("")
        };
        let mut right_lines = vec![
            Line::from(Span::styled(
                t!("profile_edit.preview_title"),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::raw("")),
        ];
        if let Some(ref args) = ui.edit_args {
            let summary = fields
                .iter()
                .map(|f| {
                    let key = format!("profile_edit.{}", f.name_key);
                    format!("  {}: {}", t!(&key), (f.display)(args))
                })
                .collect::<Vec<_>>()
                .join("\n");
            right_lines.push(Line::from(Span::styled(
                summary,
                Style::default().fg(Color::DarkGray),
            )));
        }
        right_lines.push(Line::from(Span::raw("")));
        right_lines.push(Line::from(Span::styled(
            help_text,
            Style::default().fg(Color::DarkGray),
        )));
        Paragraph::new(right_lines)
    };

    let edit_block = Block::default()
        .borders(Borders::ALL)
        .title(t!("profile_edit.edit_title"))
        .border_style(Style::default().fg(Color::Green));
    f.render_widget(right_content.block(edit_block), main[1]);

    // 底部帮助
    let help = t!("profile_edit.help");
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            help,
            Style::default().fg(Color::DarkGray),
        )))
        .alignment(ratatui::layout::Alignment::Center),
        vert[2],
    );
}

/// 处理配置编辑界面的按键事件
fn handle_profile_edit_key(ui: &mut Ui, code: KeyCode, config_path: &str) -> Option<MenuSelection> {
    let fields = profile_fields();
    let field_count = fields.len();

    if ui.field_edit_mode {
        // 字段值编辑模式
        match code {
            KeyCode::Esc => {
                ui.field_edit_mode = false;
                ui.field_edit_buf.clear();
                set_status(ui, t!("profile_edit.edit_cancelled"));
            }
            KeyCode::Enter => {
                let idx = ui.menu_list_idx.min(field_count.saturating_sub(1));
                let val = ui.field_edit_buf.clone();
                if let Some(ref mut args) = ui.edit_args {
                    match (fields[idx].apply)(args, &val) {
                        Ok(()) => {
                            ui.field_edit_mode = false;
                            ui.field_edit_buf.clear();
                            set_status(ui, t!("profile_edit.field_updated"));
                        }
                        Err(msg) => {
                            set_status(ui, msg);
                        }
                    }
                }
            }
            KeyCode::Backspace => {
                ui.field_edit_buf.pop();
            }
            KeyCode::Char(ch) => {
                ui.field_edit_buf.push(ch);
            }
            _ => {}
        }
    } else {
        // 导航模式
        match code {
            KeyCode::Up => {
                ui.menu_list_idx = ui.menu_list_idx.saturating_sub(1);
            }
            KeyCode::Down => {
                if ui.menu_list_idx + 1 < field_count {
                    ui.menu_list_idx += 1;
                }
            }
            KeyCode::Enter => {
                // 进入编辑模式
                let idx = ui.menu_list_idx.min(field_count.saturating_sub(1));
                let val = (fields[idx].display)(ui.edit_args.as_ref().unwrap());
                ui.field_edit_buf = val;
                ui.field_edit_mode = true;
            }
            KeyCode::Char('s') => {
                // 保存配置
                let name = ui.edit_profile_name.clone();
                if let Some(ref args) = ui.edit_args {
                    if let Some(ref name) = name {
                        match save_edited_profile(config_path, name, args) {
                            Ok(()) => {
                                set_status(ui, t!("profile_edit.save_success", name = name));
                                // 回退到配置管理
                                ui.menu_screen = MenuScreen::ProfileSet;
                                ui.menu_list_idx =
                                    ui.profiles.iter().position(|p| p == name).unwrap_or(0);
                                ui.edit_profile_name = None;
                                ui.edit_args = None;
                            }
                            Err(e) => {
                                set_status(ui, format!("{}", e));
                            }
                        }
                    }
                }
            }
            KeyCode::Esc => {
                // 返回配置管理，不保存
                ui.menu_screen = MenuScreen::ProfileSet;
                ui.menu_list_idx = ui
                    .profiles
                    .iter()
                    .position(|p| Some(p.as_str()) == ui.edit_profile_name.as_deref())
                    .unwrap_or(0);
                ui.edit_profile_name = None;
                ui.edit_args = None;
                set_status(ui, t!("profile_edit.edit_cancelled"));
            }
            KeyCode::Char('q') => {
                return Some(MenuSelection {
                    main_mode: MainMode::TcpClient,
                    profile_name: None,
                    quit: true,
                });
            }
            _ => {}
        }
    }
    None
}

// ─────────────────────────────────────────
// 名称输入提示：新增/克隆配置
// ─────────────────────────────────────────

/// 保存一个新配置到配置文件
fn save_new_profile(config_path: &str, name: &str, args: &Args) -> Result<()> {
    let config_str = std::fs::read_to_string(config_path).unwrap_or_default();
    let mut configs: HashMap<String, Args> = toml::from_str(&config_str).unwrap_or_default();
    configs.insert(name.to_string(), args.clone());
    let s = toml::to_string_pretty(&configs)?;
    std::fs::write(config_path, s)?;
    Ok(())
}

/// 渲染名称输入界面
fn render_name_prompt(f: &mut Frame<'_>, ui: &Ui) {
    let area = f.area();
    let vert = Layout::vertical([
        Constraint::Length(3),
        Constraint::Length(7),
        Constraint::Min(0),
    ])
    .split(area);

    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            if ui.name_prompt_is_clone {
                t!("profile_settings.clone_title")
            } else {
                t!("profile_settings.add_title")
            },
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )))
        .alignment(ratatui::layout::Alignment::Center),
        vert[0],
    );

    let prompt_block = Block::default()
        .borders(Borders::ALL)
        .title(t!("profile_settings.name_prompt"))
        .border_style(Style::default().fg(Color::Cyan));
    let input_text: Line = if ui.name_prompt_buf.is_empty() {
        Line::from(Span::styled(
            t!("profile_settings.name_placeholder"),
            Style::default(),
        ))
    } else {
        Line::from(Span::styled(ui.name_prompt_buf.as_str(), Style::default()))
    };
    f.render_widget(
        Paragraph::new(input_text)
            .block(prompt_block)
            .alignment(ratatui::layout::Alignment::Center),
        vert[1],
    );

    let help = t!("profile_settings.name_help");
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            help,
            Style::default().fg(Color::DarkGray),
        )))
        .alignment(ratatui::layout::Alignment::Center),
        vert[2],
    );
}

/// 处理名称输入界面的按键事件
fn handle_name_prompt_key(ui: &mut Ui, code: KeyCode, config_path: &str) -> Option<MenuSelection> {
    match code {
        KeyCode::Enter => {
            let name = ui.name_prompt_buf.trim().to_string();
            if name.is_empty() {
                set_status(ui, t!("profile_settings.name_empty"));
                return None;
            }
            if ui.profiles.contains(&name) {
                set_status(ui, t!("profile_settings.name_exists"));
                return None;
            }
            if name == "__default__" {
                set_status(ui, t!("profile_settings.name_reserved"));
                return None;
            }
            let args = if ui.name_prompt_is_clone {
                ui.name_prompt_clone_args.clone().unwrap_or_default()
            } else {
                Args::default()
            };
            match save_new_profile(config_path, &name, &args) {
                Ok(()) => {
                    set_status(ui, t!("profile_settings.add_success", name = &name));
                    // 重新加载配置列表
                    let mut reloaded = load_profile_list(config_path);
                    reloaded.sort();
                    ui.profiles = reloaded;
                    // 选中新配置并进入编辑模式
                    ui.edit_profile_name = Some(name.clone());
                    ui.edit_args = Some(args);
                    ui.menu_list_idx = 0;
                    ui.field_edit_mode = false;
                    ui.field_edit_buf.clear();
                    ui.menu_screen = MenuScreen::ProfileEdit;
                }
                Err(e) => {
                    set_status(ui, format!("{}", e));
                }
            }
        }
        KeyCode::Esc => {
            ui.menu_screen = MenuScreen::ProfileSet;
        }
        KeyCode::Backspace => {
            ui.name_prompt_buf.pop();
        }
        KeyCode::Char('q') => {
            return Some(MenuSelection {
                main_mode: MainMode::TcpClient,
                profile_name: None,
                quit: true,
            });
        }
        KeyCode::Char(ch) => {
            if ch.is_alphanumeric() || ch == '_' || ch == '-' {
                ui.name_prompt_buf.push(ch);
            }
        }
        _ => {}
    }
    None
}

/// 运行主菜单界面，返回用户选择的结果
pub async fn run_menu(config_path: &str, profiles: Vec<String>) -> Result<MenuSelection> {
    enable_raw_mode().context("enable raw mode")?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen).context("enter alt screen")?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).context("create terminal")?;

    let mut events = EventStream::new();
    let mut ui = Ui::new(DisplayBase::Dec, profiles);
    // 尝试读取默认配置
    if let Some(def) = load_default_profile(config_path) {
        if ui.profiles.contains(&def) {
            ui.default_profile = Some(def.clone());
            ui.selected_profile = Some(def);
        }
    }

    let res: Result<MenuSelection> = loop {
        let ev = tokio::time::timeout(Duration::from_millis(100), events.next()).await;
        let ev = match ev {
            Ok(Some(Ok(ev))) => ev,
            Ok(Some(Err(e))) => break Err(anyhow!(e).context("read event")),
            _ => {
                // 刷新界面
                let _ = terminal.draw(|f| match ui.menu_screen {
                    MenuScreen::Main => render_main_menu(f, &ui),
                    MenuScreen::ProfilePick => render_profile_pick(f, &ui, config_path),
                    MenuScreen::ProfileSet => render_profile_settings(f, &ui, config_path),
                    MenuScreen::ProfileEdit => render_profile_edit(f, &ui, config_path),
                    MenuScreen::NamePrompt => render_name_prompt(f, &ui),
                });
                continue;
            }
        };

        match ev {
            Event::Key(KeyEvent { code, kind, .. }) => {
                if kind != crossterm::event::KeyEventKind::Press {
                    continue;
                }
                let sel = match ui.menu_screen {
                    MenuScreen::Main => handle_main_menu_key(&mut ui, code),
                    MenuScreen::ProfilePick => handle_profile_pick_key(&mut ui, code, config_path),
                    MenuScreen::ProfileSet => handle_profile_set_key(&mut ui, code, config_path),
                    MenuScreen::ProfileEdit => handle_profile_edit_key(&mut ui, code, config_path),
                    MenuScreen::NamePrompt => handle_name_prompt_key(&mut ui, code, config_path),
                };
                if let Some(sel) = sel {
                    break Ok(sel);
                }
            }
            Event::Resize(_, _) => {
                let _ = terminal.draw(|f| match ui.menu_screen {
                    MenuScreen::Main => render_main_menu(f, &ui),
                    MenuScreen::ProfilePick => render_profile_pick(f, &ui, config_path),
                    MenuScreen::ProfileSet => render_profile_settings(f, &ui, config_path),
                    MenuScreen::ProfileEdit => render_profile_edit(f, &ui, config_path),
                    MenuScreen::NamePrompt => render_name_prompt(f, &ui),
                });
            }
            _ => {}
        }

        // 重绘
        let _ = terminal.draw(|f| match ui.menu_screen {
            MenuScreen::Main => render_main_menu(f, &ui),
            MenuScreen::ProfilePick => render_profile_pick(f, &ui, config_path),
            MenuScreen::ProfileSet => render_profile_settings(f, &ui, config_path),
            MenuScreen::ProfileEdit => render_profile_edit(f, &ui, config_path),
            MenuScreen::NamePrompt => render_name_prompt(f, &ui),
        });
    };

    disable_raw_mode().ok();
    execute!(terminal.backend_mut(), LeaveAlternateScreen).ok();
    terminal.show_cursor().ok();
    res
}

pub async fn run_ui(
    state: Arc<RwLock<AppState>>,
    tx: mpsc::UnboundedSender<RegCmd>,
    args: Args,
    server_status: Arc<RwLock<Option<String>>>,
) -> Result<()> {
    enable_raw_mode().context(t!("run_ui.enable_raw_mode"))?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen).context(t!("run_ui.enter_alt_screen"))?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).context(t!("run_ui.create_terminal"))?;

    let mut events = EventStream::new();
    let mut ui = Ui::new(args.base, Vec::new());

    // Monitor 模式默认开启监听面板
    if args.main_mode == "monitor" {
        ui.show_monitor = true;
    }

    let tick = Duration::from_millis(args.ui_tick_ms);
    let mut interval = tokio::time::interval(tick);

    let res: Result<()> = loop {
        tokio::select! {
                    _ = interval.tick() => {
                        let s = state.read().await;
                        let server_err = server_status.read().await.clone();
                        let is_monitor_mode = args.main_mode == "monitor";
                        terminal.draw(|f| {
                            let monitor_active = is_monitor_mode || ui.show_monitor;

                            // 纯监听模式：仅显示监听面板；否则显示寄存器表 + 可选的监听覆盖层
                            let (monitor_constraint, keep) = if is_monitor_mode {
                                (Constraint::Min(3), false)
                            } else if monitor_active {
                                (Constraint::Length(12), true)
                            } else {
                                (Constraint::Length(0), false) // 不显示
                            };

                            let constraints: Vec<Constraint> = if is_monitor_mode {
                                vec![monitor_constraint, Constraint::Length(3), Constraint::Length(3)]
                            } else if keep {
                                vec![Constraint::Min(3), monitor_constraint, Constraint::Length(3), Constraint::Length(3)]
                            } else {
                                vec![Constraint::Min(5), Constraint::Length(3), Constraint::Length(3)]
                            };

                            let areas = Layout::vertical(&constraints).split(f.area());
                            let mut area_idx = 0;

                            if is_monitor_mode {
                                // 纯监听模式：全屏监听面板
                                let monitor_area = areas[area_idx]; area_idx += 1;

                                // 水平分割：历史流（左 55%），统计表（右 45%）
                                let monitor_split = Layout::horizontal([
                                    Constraint::Percentage(55),
                                    Constraint::Percentage(45),
                                ]).split(monitor_area);

                                // 左面板：历史流水
                                let history_text = format_monitor_history(&s.monitor, ui.monitor_scroll);
                                let history_style = if ui.monitor_focus_history { Color::Yellow } else { Color::Green };
                                f.render_widget(
                                    ratatui::widgets::Paragraph::new(history_text)
                                        .block(Block::default()
                                            .borders(Borders::ALL)
                                            .title(t!("run_ui.monitor_history_title"))
                                            .border_style(Style::default().fg(history_style))
                                        )
                                        .style(Style::default().fg(Color::Green)),
                                    monitor_split[0],
                                );

                                // 右面板：统计一览
                                let stats_text = format_monitor_stats(&s.monitor);
                                let stats_style = if !ui.monitor_focus_history { Color::Yellow } else { Color::Green };
                                f.render_widget(
                                    ratatui::widgets::Paragraph::new(stats_text)
                                        .block(Block::default()
                                            .borders(Borders::ALL)
                                            .title(t!("run_ui.monitor_stats_title"))
                                            .border_style(Style::default().fg(stats_style))
                                        )
                                        .style(Style::default().fg(Color::Green)),
                                    monitor_split[1],
                                );
                            } else {
                                // Server/Client 模式：顶部区域
                                let top_area = &areas[area_idx]; area_idx += 1;

                                if ui.show_byte_panel {
                                    let top = Layout::horizontal([
                                        Constraint::Length(42),
                                        Constraint::Min(20),
                                    ]).split(*top_area);

                                    // 字节流面板
                                    if let Some(ref fi) = s.last_frame {
                                        let panel_text = format_byte_panel(fi);
                                        f.render_widget(
                                            ratatui::widgets::Paragraph::new(panel_text)
                                                .block(Block::default().borders(Borders::ALL).title(t!("run_ui.byte_panel_title")))
                                                .style(Style::default().fg(Color::Cyan)),
                                            top[0],
                                        );
                                    } else {
                                        f.render_widget(
                                            ratatui::widgets::Paragraph::new(t!("run_ui.no_data"))
                                                .block(Block::default().borders(Borders::ALL).title(t!("run_ui.byte_panel_title")))
                                                .style(Style::default().fg(Color::DarkGray)),
                                            top[0],
                                        );
                                    }

                                    render_register_table(f, &s, &mut ui, top[1]);
                                } else {
                                    render_register_table(f, &s, &mut ui, *top_area);
                                }

                                // 监听覆盖层
                                if monitor_active {
                                    let monitor_area = areas[area_idx]; area_idx += 1;
                                    let monitor_split = Layout::horizontal([
                                        Constraint::Percentage(55),
                                        Constraint::Percentage(45),
                                    ]).split(monitor_area);

                                    let history_text = format_monitor_history(&s.monitor, ui.monitor_scroll);
                                    let history_style = if ui.monitor_focus_history { Color::Yellow } else { Color::Green };
                                    f.render_widget(
                                        ratatui::widgets::Paragraph::new(history_text)
                                            .block(Block::default()
                                                .borders(Borders::ALL)
                                                .title(t!("run_ui.monitor_history_title"))
                                                .border_style(Style::default().fg(history_style))
                                            )
                                            .style(Style::default().fg(Color::Green)),
                                        monitor_split[0],
                                    );

                                    let stats_text = format_monitor_stats(&s.monitor);
                                    let stats_style = if !ui.monitor_focus_history { Color::Yellow } else { Color::Green };
                                    f.render_widget(
                                        ratatui::widgets::Paragraph::new(stats_text)
                                            .block(Block::default()
                                                .borders(Borders::ALL)
                                                .title(t!("run_ui.monitor_stats_title"))
                                                .border_style(Style::default().fg(stats_style))
                                            )
                                            .style(Style::default().fg(Color::Green)),
                                        monitor_split[1],
                                    );
                                }
                            }

                            let status_bar_index = area_idx; area_idx += 1;
                            let help_index = area_idx;

                            // --- 状态栏 ---
                            let status_line = if let Some(m) = server_err.as_deref() {
                                t!("run_ui.error_prefix", msg = m)
                            } else if let Some(m) = ui.status_msg.as_deref() {
                                std::borrow::Cow::Owned(m.to_string())
                            } else if ui.edit_mode {
                                if ui.edit_is_profile {
                                    t!("run_ui.edit_save_profile", buf = &ui.edit_buf)
                                } else if ui.edit_is_label {
                                    t!("run_ui.edit_label", buf = &ui.edit_buf)
                                } else {
                                    t!("run_ui.edit_value", base = format!("{:?}", ui.base), buf = &ui.edit_buf)
                                }
                            } else if s.stability_test_running {
                                let (total, ok, fail) = s.stability_stats;
                                t!("run_ui.status_stability", total = total, ok = ok, fail = fail)
                            } else if is_monitor_mode {
                                t!("run_ui.status_monitoring", frames = s.monitor.total_frames)
                            } else if s.monitor.total_frames > 0 && ui.show_monitor {
                                t!("run_ui.status_monitor", frames = s.monitor.total_frames)
                            } else if !s.reg_change_history.is_empty() {
                                let changes = s.reg_change_history.len();
                                let last = s.reg_change_history.last().unwrap();
                                t!("run_ui.status_reg_change", count = changes, addr = last.addr, dir = format!("{}", last.direction))
                            } else if let Some(ref fi) = s.last_frame {
                                if fi.is_tcp {
                                    t!("run_ui.status_tcp", func = &fi.func_name, base = format!("{:?}", ui.base))
                                } else {
                                    t!("run_ui.status_rtu", func = &fi.func_name, base = format!("{:?}", ui.base))
                                }
                            } else {
                                t!("run_ui.status_waiting", base = format!("{:?}", ui.base))
                            };

                            f.render_widget(
                                ratatui::widgets::Paragraph::new(status_line)
                                    .block(Block::default().borders(Borders::ALL).title(t!("run_ui.status_title")))
                                    .style(if server_err.is_some() {
                                        Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
                                    } else {
                                        Style::default()
                                    }),
                                areas[status_bar_index],
                            );

                            // --- 帮助栏 ---
                            let help = if ui.edit_mode {
                                t!("run_ui.help_edit", buf = &ui.edit_buf)
                            } else if s.stability_test_running {
                                t!("run_ui.help_stability")
                            } else if is_monitor_mode {
                                t!("run_ui.help_monitoring")
                            } else if monitor_active {
                                t!("run_ui.help_monitor")
                            } else {
                                t!("run_ui.help_normal")
                            };

                            f.render_widget(
                                ratatui::widgets::Paragraph::new(help)
                                    .block(Block::default().borders(Borders::ALL).title(t!("run_ui.help_title"))),
                                areas[help_index],
                            );
                        })?;
                    }

                    maybe_ev = events.next() => {
                        let ev = match maybe_ev {
                            Some(Ok(ev)) => ev,
                            Some(Err(e)) => break Err(anyhow!(e).context("read event")),
                            None => continue,
                        };

                        match ev {
                            Event::Key(KeyEvent { code, modifiers, kind, .. }) => {
                                if kind != crossterm::event::KeyEventKind::Press {
                                    continue;
                                }

                                if !ui.edit_mode
                                    && code == KeyCode::Char('c')
                                    && !modifiers.contains(KeyModifiers::CONTROL)
                                {
                                    *server_status.write().await = None;
                                    ui.status_msg = None;
                                }

                                if !ui.edit_mode
                                    && (code == KeyCode::Char('q')
                                        || (code == KeyCode::Char('c')
                                            && modifiers.contains(KeyModifiers::CONTROL)))
                                {
                                    break Ok(());
                                }

                                let is_monitor_mode = args.main_mode == "monitor";

                                if ui.edit_mode {
                                    match code {
                                        KeyCode::Esc => {
                                            ui.edit_mode = false;
                                            ui.edit_is_profile = false;
                                            ui.edit_buf.clear();
                                            ui.status_msg = None;
                                        }

        KeyCode::Enter => {
            if ui.edit_is_profile {
                let profile_name = ui.edit_buf.clone();
                let config_str = std::fs::read_to_string(&args.config).unwrap_or_default();
                let mut configs: HashMap<String, Args> = toml::from_str(&config_str).unwrap_or_default();

                // 提取现有的非空备注到用于保存的配置槽位中
                let mut profile_args = args.clone();
                profile_args.labels.clear();
                {
                    let s = state.read().await;
                    for (i, label) in s.holding_label.iter().enumerate() {
                        if !label.is_empty() {
                            profile_args.labels.insert(i.to_string(), label.clone());
                        }
                    }
                }

                configs.insert(profile_name.clone(), profile_args);
                match toml::to_string_pretty(&configs) {
                    Ok(s) => match std::fs::write(&args.config, s) {
                        Ok(_) => set_status(&mut ui, t!("run_ui.save_success", name = profile_name, path = &args.config)),
                        Err(e) => set_status(&mut ui, t!("run_ui.save_fail_write", err = e.to_string())),
                    },
                    Err(e) => set_status(&mut ui, t!("run_ui.save_fail_serialize", err = e.to_string())),
                }

                ui.edit_mode = false;
                ui.edit_is_profile = false;
                ui.edit_buf.clear();
            } else if ui.edit_is_label {
                let mut s = state.write().await;
                s.holding_label[ui.selected] = ui.edit_buf.clone();
                ui.edit_mode = false;
                ui.edit_buf.clear();
            } else {
                match parse_u16_str(&ui.edit_buf, ui.base) {
                    Ok(new_val) => {
                        let (resp_tx,_) = oneshot::channel();
                        let _ = tx.send(RegCmd::WriteSingleHolding {
                            addr: ui.selected,
                            value: new_val,
                            resp: resp_tx,
                        });

                        ui.edit_mode = false;
                        ui.edit_buf.clear();
                    }
                    Err(e) => {
                        set_status(&mut ui, t!("main.invalid_input_value", err = e.to_string()));
                    }
                }
            }
        }

                                        KeyCode::Backspace => {
                                            ui.edit_buf.pop();
                                            ui.status_msg = None;
                                        }

                                        KeyCode::Char('m') => {
                                            if ui.edit_is_label || ui.edit_is_profile {
                                                ui.edit_buf.push('m');
                                            } else {
                                                match parse_u16_str(&ui.edit_buf, ui.base) {
                                                    Ok(new_val) => {
                                                        let (resp_tx, resp_rx) = oneshot::channel();
                                                        let values = vec![new_val; 100];
                                                        let _ = tx.send(RegCmd::WriteMultipleHolding {
                                                            addr: ui.selected,
                                                            values,
                                                            resp: resp_tx,
                                                        });

                                                        match timeout(UI_TIMEOUT, resp_rx).await {
                                                            Ok(Ok(Ok(()))) => {
                                                                ui.edit_mode = false;
                                                                ui.edit_buf.clear();
                                                                ui.status_msg = None;
                                                            }
                                                            Ok(Ok(Err(ex))) => set_status(
                                                                &mut ui,
                                                                t!("run_ui.modbus_exception", ex = format!("{:?}", ex)),
                                                            ),
                                                            Ok(Err(_)) => set_status(
                                                                &mut ui,
                                                                t!("run_ui.worker_disconnected"),
                                                            ),
                                                            Err(_) => {
                                                                set_status(
                                                                    &mut ui,
                                                                    t!("run_ui.write_timeout"),
                                                                );
                                                                ui.edit_mode = false;
                                                                ui.edit_buf.clear();
                                                                ui.status_msg = None;
                                                            }
                                                        }
                                                    }
                                                    Err(e) => set_status(
                                                        &mut ui,
                                                        t!("run_ui.invalid_value", err = e),
                                                    ),
                                                }
                                            }
                                        }

                                        KeyCode::Char(ch) => {
                                            if ui.edit_is_label || ui.edit_is_profile {
                                                ui.edit_buf.push(ch);
                                                ui.status_msg = None;
                                            } else if edit_accepts_char(&ui.edit_buf, ch, ui.base) {
                                                ui.edit_buf.push(ch);
                                                ui.status_msg = None;
                                            } else {
                                                set_status(
                                                    &mut ui,
                                                    t!("run_ui.char_rejected"),
                                                );
                                            }
                                        }

                                        _ => {}
                                    }
                                } else {
                                    match code {
                                        KeyCode::PageDown => {
                                            let len = state.read().await.holding.len();
                                            ui.selected = len.saturating_sub(1);
                                        }
                                        KeyCode::PageUp => {
                                            ui.selected = 0;
                                        }

                                        KeyCode::Char('k') | KeyCode::Up => {
                                            if is_monitor_mode || (ui.show_monitor && ui.monitor_focus_history) {
                                                ui.monitor_scroll = ui.monitor_scroll.saturating_sub(1);
                                            } else {
                                                ui.selected = ui.selected.saturating_sub(1);
                                            }
                                        }
                                        KeyCode::Char('j') | KeyCode::Down => {
                                            if is_monitor_mode || (ui.show_monitor && ui.monitor_focus_history) {
                                                let len = state.read().await.monitor.history.len();
                                                if ui.monitor_scroll + 1 < len.saturating_sub(8) {
                                                    ui.monitor_scroll += 1;
                                                }
                                            } else {
                                                let len = state.read().await.holding.len();
                                                ui.selected = (ui.selected + 1).min(len.saturating_sub(1));
                                            }
                                        }
                                        KeyCode::Char('d') => {
                                            ui.base = DisplayBase::Dec;
                                            ui.status_msg = None;
                                        }
                                        KeyCode::Char('h') => {
                                            ui.base = DisplayBase::Hex;
                                            ui.status_msg = None;
                                        }

                                        KeyCode::Char('b') => {
                                            ui.base = DisplayBase::Bin;
                                            ui.status_msg = None;
                                        }

                                        KeyCode::Char('t') => {
                                            let s = state.read().await;
                                            if ui.selected < s.holding.len() {
                                                ui.edit_mode = true;
                                                ui.edit_is_label = true;
                                                ui.edit_is_profile = false;
                                                ui.edit_buf = s.holding_label[ui.selected].clone();
                                                ui.status_msg = None;
                                            }
                                        }
                                        KeyCode::Char('o') => {
                                            ui.edit_mode = true;
                                            ui.edit_is_profile = true;
                                            ui.edit_is_label = false;
                                            ui.edit_buf.clear();
                                            ui.status_msg = None;
                                        }
                                        KeyCode::Char('e') => {
                                            let s = state.read().await;
                                            if ui.selected < s.holding.len() {
                                                ui.edit_mode = true;
                                                ui.edit_is_label = false;
                                                ui.edit_is_profile = false;
                                                ui.edit_buf = format_u16(s.holding[ui.selected], ui.base);
                                                ui.status_msg = None;
                                            }
                                        }

                                        KeyCode::Char('B') => {
                                            ui.show_byte_panel = !ui.show_byte_panel;
                                            if ui.show_byte_panel {
                                                set_status(&mut ui, t!("run_ui.byte_panel_shown"));
                                            } else {
                                                set_status(&mut ui, t!("run_ui.byte_panel_hidden"));
                                            }
                                        }
                                        KeyCode::Char('M') => {
                                            if !is_monitor_mode {
                                                ui.show_monitor = !ui.show_monitor;
                                                if ui.show_monitor {
                                                    set_status(&mut ui, t!("run_ui.monitor_shown"));
                                                } else {
                                                    set_status(&mut ui, t!("run_ui.monitor_hidden"));
                                                }
                                            }
                                        }
                                        KeyCode::Tab => {
                                            if is_monitor_mode || ui.show_monitor {
                                                ui.monitor_focus_history = !ui.monitor_focus_history;
                                                if !ui.monitor_focus_history {
                                                    set_status(&mut ui, t!("run_ui.monitor_stats_mode"));
                                                } else {
                                                    set_status(&mut ui, t!("run_ui.monitor_history_mode"));
                                                }
                                            }
                                        }
                                        KeyCode::Char('S') => {
                                            let mut s = state.write().await;
                                            s.stability_test_running = !s.stability_test_running;
                                            if s.stability_test_running {
                                                s.stability_stats = (0, 0, 0);
                                                set_status(&mut ui, t!("run_ui.stability_started"));
                                            } else {
                                                set_status(&mut ui, t!("run_ui.stability_stopped"));
                                            }
                                        }
                                        _ => {}
                                    }
                                }
                            }
                            _ => {}
                        }
                    }

                    _ = tokio::signal::ctrl_c() => {
                        break Ok(());
                    }
                }
    };

    disable_raw_mode().ok();
    execute!(terminal.backend_mut(), LeaveAlternateScreen).ok();
    terminal.show_cursor().ok();

    res
}

/// 渲染寄存器表格（重构为可复用的辅助函数）
fn render_register_table(
    f: &mut ratatui::Frame<'_>,
    s: &AppState,
    ui: &mut Ui,
    area: ratatui::layout::Rect,
) {
    let visible_rows = area.height.saturating_sub(3) as usize;
    if ui.selected >= s.holding.len() {
        ui.selected = s.holding.len().saturating_sub(1);
    }
    if ui.selected < ui.scroll {
        ui.scroll = ui.selected;
    }
    if visible_rows > 0 && ui.selected >= ui.scroll + visible_rows {
        ui.scroll = ui.selected + 1 - visible_rows;
    }

    let header = Row::new(vec![
        Cell::from(t!("register_table.col_addr")),
        Cell::from(t!("register_table.col_label")),
        Cell::from(t!("register_table.col_value")),
        Cell::from(t!("register_table.col_change")),
    ])
    .style(
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    );

    let rows = s
        .holding
        .iter()
        .enumerate()
        .skip(ui.scroll)
        .take(visible_rows.max(1))
        .map(|(i, v)| {
            let mut val = format_u16(*v, ui.base);
            let mut label = s.holding_label[i].clone();
            if ui.edit_mode && i == ui.selected && !ui.edit_is_profile {
                if ui.edit_is_label {
                    label = ui.edit_buf.clone();
                } else {
                    val = ui.edit_buf.clone();
                }
            }
            // 变化方向指示
            let change_str = if i < s.reg_just_changed.len() && s.reg_just_changed[i] {
                format!("{}", s.reg_change_direction[i])
            } else {
                String::new()
            };
            Row::new(vec![
                Cell::from(format!("{}", i)),
                Cell::from(label),
                Cell::from(val),
                Cell::from(change_str),
            ])
        });

    let mut table_state = TableState::default();
    table_state.select(Some(ui.selected.saturating_sub(ui.scroll)));

    let t = Table::new(
        rows,
        [
            Constraint::Length(18),
            Constraint::Length(42),
            Constraint::Min(10),
            Constraint::Length(6),
        ],
    )
    .header(header)
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title(t!("register_table.title")),
    )
    .row_highlight_style(Style::default().bg(Color::Blue))
    .highlight_symbol(">> ");

    f.render_stateful_widget(t, area, &mut table_state);
}

/// 构建字节流面板的显示文本
fn format_byte_panel(fi: &FrameInfo) -> String {
    let mut text = String::new();
    let mut bytes = frame_bytes_from_info(fi);
    if bytes.is_empty() {
        return t!("byte_panel.build_fail").to_string();
    }

    let header_line = if fi.is_tcp {
        t!("byte_panel.tcp_response", func = &fi.func_name)
    } else {
        t!("byte_panel.rtu_response", func = &fi.func_name)
    };
    text.push_str(&format!("{}\n", header_line));
    text.push_str("━━━━━━━━━━━━━━━━━\n");
    text.push_str(&format!("{}\n", t!("byte_panel.header")));

    if fi.is_tcp {
        // TCP 帧头
        let trans_id = u16::from_be_bytes([bytes[0], bytes[1]]);
        let proto_id = u16::from_be_bytes([bytes[2], bytes[3]]);
        let len = u16::from_be_bytes([bytes[4], bytes[5]]);
        let unit = bytes[6];
        text.push_str(&format!(
            "0-1   {:04X}  {}\n",
            trans_id,
            t!("byte_panel.transaction_id", id = trans_id)
        ));
        let proto_desc = if proto_id == 0 {
            t!("byte_panel.protocol_id_modbus")
        } else {
            t!("byte_panel.protocol_id_other")
        };
        text.push_str(&format!("2-3   {:04X}  {}\n", proto_id, proto_desc));
        text.push_str(&format!(
            "4-5   {:04X}  {}\n",
            len,
            t!("byte_panel.length", len = len)
        ));
        text.push_str(&format!(
            "6     {:02X}    {}\n",
            unit,
            t!("byte_panel.unit", unit = unit)
        ));

        // 从字节7开始是功能码
        let func = bytes[7];
        text.push_str(&format!(
            "7     {:02X}    {}\n",
            func,
            t!("byte_panel.func_code", name = &fi.func_name, code = func)
        ));

        // 截取剩余部分作为原始帧
        bytes = bytes[8..].to_vec();
    } else {
        // RTU 帧: [unit] [func] [data...] [crc]
        let unit = bytes[0];
        let func = bytes[1];
        text.push_str(&format!(
            "0     {:02X}    {}\n",
            unit,
            t!("byte_panel.unit", unit = unit)
        ));
        text.push_str(&format!(
            "1     {:02X}    {}\n",
            func,
            t!("byte_panel.func_code", name = &fi.func_name, code = func)
        ));

        // 跳过地址和功能码，保留数据+CRC
        bytes = bytes[2..].to_vec();
    }

    // 解析剩余数据
    let func_body = &bytes;

    if func_body.len() >= 2 {
        match fi.func_code {
            0x03 => {
                // 响应: [byte_count] [data...] [crc...]
                let bc = func_body[0] as usize;
                let offset_base = if fi.is_tcp { 8usize } else { 2usize };
                text.push_str(&format!(
                    "{:<5} {:02X}    {}\n",
                    if fi.is_tcp { 8 } else { 2 },
                    bc,
                    t!("byte_panel.byte_count", count = bc)
                ));
                let mut data_offset = 1;
                let mut reg_idx = 0;
                while data_offset + 1 < func_body.len().saturating_sub(2)
                    && reg_idx < fi.values.len()
                {
                    if data_offset + 1 >= func_body.len() {
                        break;
                    }
                    let h = func_body[data_offset];
                    let l = func_body[data_offset + 1];
                    let val = u16::from_be_bytes([h, l]);
                    let abs_offset = offset_base + data_offset;
                    text.push_str(&format!(
                        "{}-{} {:04X}  {}\n",
                        abs_offset,
                        abs_offset + 1,
                        val,
                        t!("byte_panel.register", idx = reg_idx, val = val)
                    ));
                    data_offset += 2;
                    reg_idx += 1;
                }
                // CRC (最后2字节)
                if func_body.len() >= 2 {
                    let crc_start = func_body.len() - 2;
                    let crc_val =
                        u16::from_le_bytes([func_body[crc_start], func_body[crc_start + 1]]);
                    let abs_crc = offset_base + crc_start;
                    text.push_str(&format!(
                        "{}-{} {:04X}  {}\n",
                        abs_crc,
                        abs_crc + 1,
                        crc_val,
                        t!("byte_panel.crc16", crc = crc_val)
                    ));
                }
            }
            0x06 => {
                // 写单寄存器响应: [addr hi] [addr lo] [val hi] [val lo] [crc...]
                let offset_base = if fi.is_tcp { 8usize } else { 2usize };
                if func_body.len() >= 4 {
                    let addr_val = u16::from_be_bytes([func_body[0], func_body[1]]);
                    let data_val = u16::from_be_bytes([func_body[2], func_body[3]]);
                    text.push_str(&format!(
                        "{}-{} {:04X}  {}\n",
                        offset_base,
                        offset_base + 1,
                        addr_val,
                        t!("byte_panel.register_addr", addr = addr_val)
                    ));
                    text.push_str(&format!(
                        "{}-{} {:04X}  {}\n",
                        offset_base + 2,
                        offset_base + 3,
                        data_val,
                        t!("byte_panel.register_val", val = data_val)
                    ));
                    if !fi.is_tcp && func_body.len() >= 6 {
                        let crc_start = func_body.len() - 2;
                        let crc_val =
                            u16::from_le_bytes([func_body[crc_start], func_body[crc_start + 1]]);
                        let abs_crc = offset_base + crc_start;
                        text.push_str(&format!(
                            "{}-{} {:04X}  {}\n",
                            abs_crc,
                            abs_crc + 1,
                            crc_val,
                            t!("byte_panel.crc16", crc = crc_val)
                        ));
                    }
                }
            }
            0x10 => {
                // 写多寄存器响应: [addr hi] [addr lo] [qty hi] [qty lo] [crc...]
                let offset_base = if fi.is_tcp { 8usize } else { 2usize };
                if func_body.len() >= 4 {
                    let addr_val = u16::from_be_bytes([func_body[0], func_body[1]]);
                    let qty = u16::from_be_bytes([func_body[2], func_body[3]]);
                    text.push_str(&format!(
                        "{}-{} {:04X}  {}\n",
                        offset_base,
                        offset_base + 1,
                        addr_val,
                        t!("byte_panel.start_addr", addr = addr_val)
                    ));
                    text.push_str(&format!(
                        "{}-{} {:04X}  {}\n",
                        offset_base + 2,
                        offset_base + 3,
                        qty,
                        t!("byte_panel.register_qty", qty = qty)
                    ));
                    if !fi.is_tcp && func_body.len() >= 6 {
                        let crc_start = func_body.len() - 2;
                        let crc_val =
                            u16::from_le_bytes([func_body[crc_start], func_body[crc_start + 1]]);
                        let abs_crc = offset_base + crc_start;
                        text.push_str(&format!(
                            "{}-{} {:04X}  {}\n",
                            abs_crc,
                            abs_crc + 1,
                            crc_val,
                            t!("byte_panel.crc16", crc = crc_val)
                        ));
                    }
                }
            }
            _ => {}
        }
    }

    // 原始帧
    text.push_str("━━━━━━━━━━━━━━━━━\n");
    let full_bytes = frame_bytes_from_info(fi);
    let hex_str: Vec<String> = full_bytes.iter().map(|b| format!("{:02X}", b)).collect();
    let lines = hex_str.chunks(8);
    text.push_str(&format!("{}\n", t!("byte_panel.raw_frame")));
    for line in lines {
        text.push_str(&format!("  {}\n", line.join(" ")));
    }

    text
}

/// 格式化监听历史流水
fn format_monitor_history(m: &MonitorStats, scroll: usize) -> String {
    const MAX_LINES: usize = 8;
    let total = m.history.len();
    let start = if total > scroll + MAX_LINES {
        total - MAX_LINES - scroll
    } else {
        0
    };
    let mut text = String::new();
    for rec in m.history.iter().skip(start).rev().take(MAX_LINES) {
        let dir = if rec.is_request { "⇒" } else { "⇐" };
        let tag = if rec.is_tcp { "TCP" } else { "RTU" };
        text.push_str(&format!(
            "{} {} {} {} addr=0x{:04X}\n",
            rec.human_time, dir, tag, rec.func_name, rec.addr
        ));
    }
    if text.is_empty() {
        text.push_str(&t!("run_ui.no_data"));
    }
    text
}

/// 格式化监听统计一览
fn format_monitor_stats(m: &MonitorStats) -> String {
    let mut text = String::new();
    text.push_str(&format!(
        "{}: {}\n",
        t!("run_ui.monitor_total_frames"),
        m.total_frames
    ));
    text.push_str(&format!("{}\n", t!("run_ui.monitor_func_header")));
    if m.func_count.is_empty() {
        text.push_str(&format!("  {}\n", t!("run_ui.no_data")));
    } else {
        let mut funcs: Vec<_> = m.func_count.iter().collect();
        funcs.sort_by(|a, b| b.1.cmp(a.1));
        for (code, count) in funcs {
            text.push_str(&format!("  0x{:02X}: {}\n", code, count));
        }
    }
    text.push_str(&format!("{}\n", t!("run_ui.monitor_addr_header")));
    if m.addr_count.is_empty() {
        text.push_str(&format!("  {}\n", t!("run_ui.no_data")));
    } else {
        let mut addrs: Vec<_> = m.addr_count.iter().collect();
        addrs.sort_by(|a, b| b.1.cmp(a.1));
        for (addr, count) in addrs.iter().take(10) {
            text.push_str(&format!("  0x{:04X}: {}\n", addr, count));
        }
    }
    text
}
