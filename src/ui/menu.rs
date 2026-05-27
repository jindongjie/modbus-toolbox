use super::{set_status, MenuScreen, MenuSelection, Ui};
use crate::{Args, MainMode, RegDataFormat};
use anyhow::{anyhow, Context, Result};
use crossterm::{
    event::{Event, EventStream, KeyCode, KeyEvent},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use futures::StreamExt;
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame, Terminal,
};
use std::borrow::Cow;
use std::collections::HashMap;
use std::io;
use tokio::time::Duration;

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
    let default_args = Args {
        main_mode: name.to_string(),
        ..Default::default()
    };
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

/// 根据 Args 中的传输层类型返回监视模式标签
pub(crate) fn profile_monitor_mode_label(args: &Args) -> &'static str {
    if args.main_mode.to_ascii_lowercase().contains("tcp") {
        "tcp-monitor"
    } else {
        "rtu-monitor"
    }
}

/// 生成配置的单行简介文本
pub(crate) fn profile_pick_brief(args: &Args) -> String {
    let mode_short = match args.main_mode.to_ascii_lowercase().as_str() {
        "tcp-server" => "TCP-S",
        "tcp-client" => "TCP-C",
        "rtu-server" => "RTU-S",
        "rtu-client" => "RTU-C",
        _ => &args.main_mode,
    };
    let unit = format!("slv {}", args.unit);
    if args.main_mode.to_ascii_lowercase().contains("tcp") {
        format!(
            "{} | port {} | {} | hld {}",
            mode_short, args.tcp_port, unit, args.holding_count
        )
    } else {
        let parity = args.parity.to_uppercase();
        let baud = args.baudrate;
        format!(
            "{} | {} | {}-{}{}{} | {} | hld {}",
            mode_short,
            args.device,
            baud,
            parity,
            args.databits,
            args.stopbits,
            unit,
            args.holding_count
        )
    }
}

/// 读取配置文件，筛选出与 pending_mode 匹配的配置并生成简介
fn load_pick_list(config_path: &str, pending_mode: Option<MainMode>) -> (Vec<String>, Vec<String>) {
    let config_str = std::fs::read_to_string(config_path).unwrap_or_default();
    let configs: HashMap<String, Args> = toml::from_str(&config_str).unwrap_or_default();
    let mode_filter = pending_mode.map(|m| match m {
        MainMode::TcpServer => "tcp-server",
        MainMode::TcpClient => "tcp-client",
        MainMode::RTUServer => "rtu-server",
        MainMode::RTUClient => "rtu-client",
        MainMode::TcpMonitor => "tcp-",
        MainMode::RtuMonitor => "rtu-",
    });
    let mut entries: Vec<(String, String)> = configs
        .into_iter()
        .filter(|(name, _)| name != "__default__")
        .filter(|(_, args)| mode_filter.is_none_or(|f| args.main_mode.starts_with(f)))
        .map(|(name, args)| {
            let brief = profile_pick_brief(&args);
            (name, brief)
        })
        .collect();
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    let names: Vec<String> = entries.iter().map(|(n, _)| n.clone()).collect();
    let briefs: Vec<String> = entries.into_iter().map(|(_, b)| b).collect();
    (names, briefs)
}

/// 统计各模式下的配置数量（供主菜单显示）
fn count_profiles_by_mode(config_path: &str) -> [usize; 6] {
    let config_str = std::fs::read_to_string(config_path).unwrap_or_default();
    let configs: HashMap<String, Args> = toml::from_str(&config_str).unwrap_or_default();
    let mut counts = [0usize; 6];
    for (name, args) in &configs {
        if name == "__default__" {
            continue;
        }
        match args.main_mode.to_ascii_lowercase().as_str() {
            "tcp-server" => counts[0] += 1,
            "tcp-client" => counts[1] += 1,
            "rtu-server" => counts[2] += 1,
            "rtu-client" => counts[3] += 1,
            _ if args.main_mode.to_ascii_lowercase().contains("tcp") => counts[4] += 1,
            _ if args.main_mode.to_ascii_lowercase().contains("rtu") => counts[5] += 1,
            _ => {}
        }
    }
    counts
}

fn render_main_menu(f: &mut Frame<'_>, ui: &Ui) {
    let area = f.area();
    let vert = Layout::vertical([
        Constraint::Length(7), // Logo + version
        Constraint::Min(12),   // 菜单项
        Constraint::Length(3), // 底部信息栏
    ])
    .split(area);

    // --- Logo 区（渐显动画，固定青色 + 粗体） ---
    let logo_style = Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD);
    let logo_text: Vec<Line> = ui
        .logo_current
        .iter()
        .map(|l| Line::from(Span::styled(l.clone(), logo_style)))
        .collect();
    f.render_widget(
        Paragraph::new(logo_text).alignment(ratatui::layout::Alignment::Center),
        vert[0],
    );

    // --- 主菜单区：垂直排列 ---
    let menu_items: [(&str, MainMode); 6] = [
        ("main_menu.tcp_server", MainMode::TcpServer),
        ("main_menu.tcp_client", MainMode::TcpClient),
        ("main_menu.rtu_server", MainMode::RTUServer),
        ("main_menu.rtu_client", MainMode::RTUClient),
        ("main_menu.tcp_monitor", MainMode::TcpMonitor),
        ("main_menu.rtu_monitor", MainMode::RtuMonitor),
    ];
    // 拉取配置列表统计各模式配置数
    let counts = count_profiles_by_mode(&ui.config_path);

    let block = Block::default()
        .borders(Borders::ALL)
        .title(t!("main_menu.title"))
        .border_style(Style::default().fg(Color::Cyan));

    let inner = block.inner(vert[1]);
    f.render_widget(block, vert[1]);

    let mut lines: Vec<Line> = Vec::new();

    // 渲染 5 个模式菜单项
    for (i, &(key, _mode)) in menu_items.iter().enumerate() {
        let is_selected = i == ui.menu_list_idx;
        let count = counts.get(i).copied().unwrap_or(0);
        let label = format!("[{}] {} ({})", i + 1, t!(key), count);
        let item_style = if is_selected {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        let prefix = if is_selected { " ▸ " } else { "   " };
        lines.push(Line::from(Span::styled(
            format!("{}{}", prefix, label),
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
        format!("{}[7] {}", prefix, t!("main_menu.profile_settings")),
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
fn render_profile_pick(f: &mut Frame<'_>, ui: &Ui, _config_path: &str) {
    let area = f.area();
    let vert = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(5),
        Constraint::Length(3),
    ])
    .split(area);

    let mode_name = ui
        .pending_mode
        .map(|m| match m {
            MainMode::TcpServer => "TCP Server",
            MainMode::TcpClient => "TCP Client",
            MainMode::RTUServer => "RTU Server",
            MainMode::RTUClient => "RTU Client",
            MainMode::TcpMonitor => "TCP Monitor",
            MainMode::RtuMonitor => "RTU Monitor",
        })
        .unwrap_or("?");
    let title = format!(
        "Select Profile — {mode_name} ({} profiles)",
        ui.pick_names.len()
    );
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            title,
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )))
        .alignment(ratatui::layout::Alignment::Center),
        vert[0],
    );

    let main =
        Layout::horizontal([Constraint::Percentage(55), Constraint::Percentage(45)]).split(vert[1]);

    // 左：筛选后的配置列表（含简介）
    let mut items: Vec<Line> = Vec::new();
    for (i, name) in ui.pick_names.iter().enumerate() {
        let is_default = Some(name.as_str()) == ui.default_profile.as_deref();
        let icon = if is_default { "●" } else { "○" };
        let brief = ui.pick_briefs.get(i).map(|s| s.as_str()).unwrap_or("");
        if i == ui.menu_list_idx {
            items.push(Line::from(Span::styled(
                format!(" {} {}", icon, name),
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )));
            items.push(Line::from(Span::styled(
                format!("   {}", brief),
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::DIM),
            )));
        } else if is_default {
            items.push(Line::from(Span::styled(
                format!(" {} {}", icon, name),
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            )));
            items.push(Line::from(Span::styled(
                format!("   {}", brief),
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::DIM),
            )));
        } else {
            items.push(Line::from(Span::styled(
                format!(" {} {}", icon, name),
                Style::default(),
            )));
            items.push(Line::from(Span::styled(
                format!("   {}", brief),
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::DIM),
            )));
        }
    }
    if ui.pick_names.is_empty() {
        items.push(Line::from(Span::styled(
            t!("profile_settings.empty_list"),
            Style::default().fg(Color::DarkGray),
        )));
        let empty_msg = if ui.profiles.is_empty() {
            t!("profile_pick.no_profiles_hint")
        } else {
            t!("profile_pick.no_match_hint")
        };
        items.push(Line::from(Span::styled(
            empty_msg,
            Style::default().fg(Color::DarkGray),
        )));
    }
    let list_block = Block::default()
        .borders(Borders::ALL)
        .title(t!("profile_settings.list_title"))
        .border_style(Style::default().fg(Color::Cyan));
    f.render_widget(Paragraph::new(items).block(list_block), main[0]);

    // 右：选中配置预览
    let right_content = if !ui.pick_names.is_empty() && ui.menu_list_idx < ui.pick_names.len() {
        let sel_name = &ui.pick_names[ui.menu_list_idx];
        let config_str = std::fs::read_to_string(_config_path).unwrap_or_default();
        let configs: HashMap<String, Args> = toml::from_str(&config_str).unwrap_or_default();
        let mut lines = Vec::new();
        if let Some(args) = configs.get(sel_name.as_str()) {
            lines.push(Line::from(Span::styled(
                t!("profile_pick.preview_title"),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )));
            lines.push(Line::from(Span::raw("")));
            lines.push(Line::from(Span::styled(
                format!("  {}", t!("profile_pick.preview_name", name = sel_name)),
                Style::default().fg(Color::Green),
            )));
            lines.push(Line::from(Span::styled(
                format!(
                    "  {}",
                    t!("profile_pick.preview_mode", mode = &args.main_mode)
                ),
                Style::default(),
            )));
            lines.push(Line::from(Span::styled(
                format!("  {}", t!("profile_pick.preview_unit", unit = args.unit)),
                Style::default(),
            )));
            lines.push(Line::from(Span::styled(
                format!(
                    "  {}",
                    t!("profile_pick.preview_count", count = args.holding_count)
                ),
                Style::default(),
            )));
            if args.main_mode.to_ascii_lowercase().contains("tcp") {
                lines.push(Line::from(Span::styled(
                    format!(
                        "  {}",
                        t!("profile_pick.preview_port", port = args.tcp_port)
                    ),
                    Style::default(),
                )));
            } else {
                lines.push(Line::from(Span::styled(
                    format!(
                        "  {}",
                        t!("profile_pick.preview_device", device = args.device)
                    ),
                    Style::default(),
                )));
                lines.push(Line::from(Span::styled(
                    format!(
                        "  {}",
                        t!("profile_pick.preview_baud", baud = args.baudrate)
                    ),
                    Style::default(),
                )));
            }
            // coils / discrete / input
            lines.push(Line::from(Span::styled(
                format!(
                    "  coils:{} disc:{} input:{}",
                    args.coil_count, args.discrete_count, args.input_count
                ),
                Style::default().fg(Color::DarkGray),
            )));
        } else {
            lines.push(Line::from(Span::styled(
                t!("profile_pick.load_fail"),
                Style::default().fg(Color::Red),
            )));
        }
        Paragraph::new(lines)
    } else {
        Paragraph::new(Line::from(Span::styled(
            t!("profile_pick.select_hint"),
            Style::default().fg(Color::DarkGray),
        )))
    };

    let prev_block = Block::default()
        .borders(Borders::ALL)
        .title(t!("profile_pick.preview_title"))
        .border_style(Style::default().fg(Color::Green));
    f.render_widget(right_content.block(prev_block), main[1]);

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
        Constraint::Length(6),
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

fn handle_main_menu_key(ui: &mut Ui, code: KeyCode, config_path: &str) -> Option<MenuSelection> {
    const ITEM_COUNT: usize = 7; // TCP Server(0), TCP Client(1), RTU Server(2), RTU Client(3), TCP Monitor(4), RTU Monitor(5), Profile Settings(6)

    fn enter_pick(ui: &mut Ui, config_path: &str, mode: MainMode) {
        ui.pending_mode = Some(mode);
        let (names, briefs) = load_pick_list(config_path, ui.pending_mode);
        ui.pick_names = names;
        ui.pick_briefs = briefs;
        ui.menu_screen = MenuScreen::ProfilePick;
        ui.menu_list_idx = 0;
    }

    match code {
        KeyCode::Up => {
            ui.menu_list_idx = ui.menu_list_idx.saturating_sub(1);
        }
        KeyCode::Down => {
            if ui.menu_list_idx + 1 < ITEM_COUNT {
                ui.menu_list_idx += 1;
            }
        }
        KeyCode::Char(c) if c.is_ascii_digit() => {
            let n = (c as u8 - b'1') as usize;
            match n {
                0 => enter_pick(ui, config_path, MainMode::TcpServer),
                1 => enter_pick(ui, config_path, MainMode::TcpClient),
                2 => enter_pick(ui, config_path, MainMode::RTUServer),
                3 => enter_pick(ui, config_path, MainMode::RTUClient),
                4 => enter_pick(ui, config_path, MainMode::TcpMonitor),
                5 => enter_pick(ui, config_path, MainMode::RtuMonitor),
                6 => {
                    ui.menu_screen = MenuScreen::ProfileSet;
                    ui.menu_list_idx = 0;
                }
                _ => {}
            }
        }
        KeyCode::Enter => match ui.menu_list_idx {
            0 => enter_pick(ui, config_path, MainMode::TcpServer),
            1 => enter_pick(ui, config_path, MainMode::TcpClient),
            2 => enter_pick(ui, config_path, MainMode::RTUServer),
            3 => enter_pick(ui, config_path, MainMode::RTUClient),
            4 => enter_pick(ui, config_path, MainMode::TcpMonitor),
            5 => enter_pick(ui, config_path, MainMode::RtuMonitor),
            6 => {
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
fn handle_profile_pick_key(
    ui: &mut Ui,
    code: KeyCode,
    _config_path: &str,
) -> Option<MenuSelection> {
    match code {
        KeyCode::Up => {
            if !ui.pick_names.is_empty() {
                ui.menu_list_idx = ui.menu_list_idx.saturating_sub(1);
            }
        }
        KeyCode::Down => {
            if ui.menu_list_idx + 1 < ui.pick_names.len() {
                ui.menu_list_idx += 1;
            }
        }
        KeyCode::Enter => {
            let mode = ui.pending_mode.unwrap_or(MainMode::TcpClient);
            let profile = if ui.menu_list_idx < ui.pick_names.len() {
                Some(ui.pick_names[ui.menu_list_idx].clone())
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
        KeyCode::Char('d') => {
            // 删除选中配置（需两次确认）
            if ui.menu_list_idx < ui.profiles.len() {
                let name = ui.profiles[ui.menu_list_idx].clone();
                let confirm_key = t!("profile_settings.delete_confirm", name = &name);
                let is_confirming = ui.status_msg.as_deref() == Some(confirm_key.as_ref());
                if is_confirming {
                    // 第二次按 d：执行删除
                    let config_str = std::fs::read_to_string(config_path).unwrap_or_default();
                    let mut configs: HashMap<String, Args> =
                        toml::from_str(&config_str).unwrap_or_default();
                    configs.remove(&name);
                    if let Ok(s) = toml::to_string_pretty(&configs) {
                        if std::fs::write(config_path, s).is_ok() {
                            // 重新加载列表
                            let mut reloaded = load_profile_list(config_path);
                            reloaded.sort();
                            ui.profiles = reloaded;
                            ui.menu_list_idx =
                                ui.menu_list_idx.min(ui.profiles.len().saturating_sub(1));
                            // 如果删除的是默认配置，清除默认
                            if ui.default_profile.as_deref() == Some(&name) {
                                ui.default_profile = None;
                                let _ = save_default_profile(config_path, "");
                            }
                            set_status(ui, t!("profile_settings.delete_success", name = &name));
                        } else {
                            set_status(ui, t!("profile_settings.delete_fail"));
                        }
                    } else {
                        set_status(ui, t!("profile_settings.delete_fail"));
                    }
                } else {
                    // 第一次按 d：显示确认提示
                    set_status(ui, confirm_key);
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

type FieldApply = Box<dyn Fn(&mut Args, &str) -> Result<(), String>>;
type FieldDisplay = Box<dyn Fn(&Args) -> String>;

/// 可编辑的配置字段描述
#[allow(clippy::type_complexity)]
struct ProfileField {
    /// 字段名（i18n key 后缀）
    name_key: &'static str,
    /// 字段适用模式: "tcp", "rtu", "all"
    mode: &'static str,
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
            "tcp-monitor" | "tm" => a.main_mode = "tcp-monitor".into(),
            "rtu-server" | "rs" => a.main_mode = "rtu-server".into(),
            "rtu-client" | "rc" => a.main_mode = "rtu-client".into(),
            "rtu-monitor" | "rm" => a.main_mode = "rtu-monitor".into(),
            _ => return Err(format!("无效模式: {v} (ts/tc/tm/rs/rc/rm)")),
        }
        Ok(())
    }
    fn num_display<T: ToString + 'static>(f: fn(&Args) -> T) -> FieldDisplay {
        Box::new(move |a| f(a).to_string())
    }
    fn num_apply<T: std::str::FromStr + 'static>(
        set: fn(&mut Args, T),
        name: &'static str,
    ) -> FieldApply {
        Box::new(move |a, v| {
            let v = v.trim();
            v.parse::<T>()
                .map(|val| set(a, val))
                .map_err(|_| format!("{name} 必须是有效数字"))
        })
    }
    fn str_apply(set: fn(&mut Args, String)) -> FieldApply {
        Box::new(move |a, v| {
            set(a, v.to_string());
            Ok(())
        })
    }

    vec![
        ProfileField {
            name_key: "main_mode",
            mode: "all",
            display: Box::new(main_mode_display),
            apply: Box::new(main_mode_apply),
        },
        ProfileField {
            name_key: "tcp_port",
            mode: "tcp",
            display: num_display(|a: &Args| a.tcp_port),
            apply: num_apply(|a: &mut Args, v| a.tcp_port = v, "TCP端口"),
        },
        ProfileField {
            name_key: "unit",
            mode: "all",
            display: num_display(|a: &Args| a.unit),
            apply: num_apply(|a: &mut Args, v| a.unit = v, "从站地址"),
        },
        ProfileField {
            name_key: "holding_count",
            mode: "all",
            display: num_display(|a: &Args| a.holding_count),
            apply: num_apply(|a: &mut Args, v| a.holding_count = v, "寄存器数"),
        },
        ProfileField {
            name_key: "client_tick_ms",
            mode: "all",
            display: num_display(|a: &Args| a.client_tick_ms),
            apply: num_apply(|a: &mut Args, v| a.client_tick_ms = v, "轮询间隔"),
        },
        ProfileField {
            name_key: "device",
            mode: "rtu",
            display: Box::new(|a: &Args| a.device.clone()),
            apply: str_apply(|a: &mut Args, v| a.device = v),
        },
        ProfileField {
            name_key: "baudrate",
            mode: "rtu",
            display: num_display(|a: &Args| a.baudrate),
            apply: num_apply(|a: &mut Args, v| a.baudrate = v, "波特率"),
        },
        ProfileField {
            name_key: "parity",
            mode: "rtu",
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
            mode: "rtu",
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
            mode: "rtu",
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
            mode: "rtu",
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
fn render_profile_edit(f: &mut Frame<'_>, ui: &Ui, _config_path: &str) {
    let area = f.area();
    let vert = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(10),
        Constraint::Length(6),
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

    // 左：字段列表（含模式标签）
    let mut lines: Vec<Line> = Vec::new();
    for (i, field) in fields.iter().enumerate() {
        let is_selected = i == ui.menu_list_idx;
        let args = ui.edit_args.as_ref().unwrap();
        let val = (field.display)(args);
        let label_key = format!("profile_edit.{}", field.name_key);
        let label = t!(&label_key);
        let mode_tag = match field.mode {
            "tcp" => t!("profile_edit.field_mode_tcp"),
            "rtu" => t!("profile_edit.field_mode_rtu"),
            _ => t!("profile_edit.field_mode_all"),
        };

        let line = if is_selected {
            Line::from(vec![
                Span::styled(" ▸ ", Style::default().fg(Color::Yellow)),
                Span::styled(
                    format!("{}: ", label),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::styled(val, Style::default().fg(Color::Black).bg(Color::Cyan)),
                Span::raw(" "),
                Span::styled(
                    mode_tag,
                    Style::default()
                        .fg(Color::DarkGray)
                        .add_modifier(Modifier::DIM),
                ),
            ])
        } else {
            Line::from(vec![
                Span::raw("   "),
                Span::styled(format!("{}: ", label), Style::default()),
                Span::styled(val, Style::default().fg(Color::Green)),
                Span::raw(" "),
                Span::styled(
                    mode_tag,
                    Style::default()
                        .fg(Color::DarkGray)
                        .add_modifier(Modifier::DIM),
                ),
            ])
        };
        lines.push(line);
    }

    let list_block = Block::default()
        .borders(Borders::ALL)
        .title(t!("profile_edit.fields_title"))
        .border_style(Style::default().fg(Color::Cyan));
    f.render_widget(
        Paragraph::new(lines)
            .block(list_block)
            .wrap(Wrap { trim: false }),
        main[0],
    );

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

        // 如果是 device 字段编辑，显示可用串口列表
        if field.name_key == "device" && !ui.serial_ports.is_empty() {
            edit_lines.push(Line::from(Span::styled(
                t!("profile_edit.device_port_list"),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )));
            for port in ui.serial_ports.iter() {
                let selected_port = &ui.serial_ports[ui.serial_port_idx % ui.serial_ports.len()];
                if port == selected_port {
                    edit_lines.push(Line::from(vec![
                        Span::styled(" ● ", Style::default().fg(Color::Cyan)),
                        Span::styled(
                            port.clone(),
                            Style::default()
                                .fg(Color::Cyan)
                                .add_modifier(Modifier::BOLD),
                        ),
                    ]));
                } else {
                    edit_lines.push(Line::from(vec![
                        Span::raw(" ○ "),
                        Span::styled(port.clone(), Style::default().fg(Color::DarkGray)),
                    ]));
                }
            }
            edit_lines.push(Line::from(Span::raw("")));
            edit_lines.push(Line::from(Span::styled(
                t!("profile_edit.serial_hint"),
                Style::default().fg(Color::DarkGray),
            )));
        } else if field.name_key == "device" && ui.serial_ports.is_empty() {
            edit_lines.push(Line::from(Span::styled(
                t!("profile_edit.device_no_ports"),
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::DIM),
            )));
            edit_lines.push(Line::from(Span::raw("")));
        }

        // 如有校验提示，添加提示信息
        let hint = match field.name_key {
            "main_mode" => "ts/tc/tm/rs/rc/rm 或 tcp-client/tcp-server/tcp-monitor/rtu-client/rtu-server/rtu-monitor",
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
        Paragraph::new(edit_lines).wrap(Wrap { trim: false })
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
        Paragraph::new(right_lines).wrap(Wrap { trim: false })
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
        .wrap(Wrap { trim: false })
        .alignment(ratatui::layout::Alignment::Center),
        vert[2],
    );
}
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
            KeyCode::Tab | KeyCode::Down => {
                // 在 device 字段编辑时，Tab/↓ 切换到下一个可用串口
                let idx = ui.menu_list_idx.min(field_count.saturating_sub(1));
                if fields[idx].name_key == "device" && !ui.serial_ports.is_empty() {
                    let len = ui.serial_ports.len();
                    ui.serial_port_idx = (ui.serial_port_idx + 1) % len;
                    ui.field_edit_buf = ui.serial_ports[ui.serial_port_idx].clone();
                }
            }
            KeyCode::Up => {
                // 在 device 字段编辑时，↑ 切换到上一个可用串口
                let idx = ui.menu_list_idx.min(field_count.saturating_sub(1));
                if fields[idx].name_key == "device" && !ui.serial_ports.is_empty() {
                    let len = ui.serial_ports.len();
                    ui.serial_port_idx = (ui.serial_port_idx + len - 1) % len;
                    ui.field_edit_buf = ui.serial_ports[ui.serial_port_idx].clone();
                }
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

// ─────────────────────────────────────────
// 菜单主循环
// ─────────────────────────────────────────

const LOGO_ANIM_FRAMES: u32 = 45;

/// Logo 动画插帧
fn logo_animate_frame(current: &mut [String], target: &[String], frame: u32) {
    let total = LOGO_ANIM_FRAMES;
    for (i, line) in current.iter_mut().enumerate() {
        if let Some(tgt) = target.get(i) {
            let len = tgt.len();
            let progress = (len as u64) * (frame as u64) / (total as u64);
            let progress = progress.min(len as u64) as usize;
            line.clear();
            line.push_str(&tgt[..progress]);
        }
    }
}

pub async fn run_menu(config_path: &str, profiles: Vec<String>) -> Result<MenuSelection> {
    enable_raw_mode().context("enable raw mode")?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen).context("enter alt screen")?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).context("create terminal")?;

    let mut events = EventStream::new();
    let mut ui = Ui::new(RegDataFormat::default(), profiles);
    ui.config_path = config_path.to_string();
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
                // Logo 动画（约 4.5 秒完成，LOGO_ANIM_FRAMES × 100ms）
                if ui.logo_frame < LOGO_ANIM_FRAMES {
                    ui.logo_frame += 1;
                    logo_animate_frame(&mut ui.logo_current, &ui.logo_target, ui.logo_frame);
                }
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
                    MenuScreen::Main => handle_main_menu_key(&mut ui, code, config_path),
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
