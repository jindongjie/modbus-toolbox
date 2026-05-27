use super::{centered_rect, Ui};
use crate::ui::menu::profile_monitor_mode_label;
use crate::ui::profile_pick_brief;
use crate::{Args, MainMode, MonitorStats};
use ratatui::{
    layout::{Constraint, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};
use std::collections::HashMap;

pub(crate) fn render_monitor_profile_pick(f: &mut Frame<'_>, ui: &Ui, _config_path: &str) {
    let area = f.area();
    let vert = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(5),
        Constraint::Length(6),
    ])
    .split(area);

    let title = t!("run_ui.monitor_profile_pick_title");
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

    // 过滤配置列表：只显示与当前监听模式传输层匹配的配置
    let config_str = std::fs::read_to_string(_config_path).unwrap_or_default();
    let configs: HashMap<String, Args> = toml::from_str(&config_str).unwrap_or_default();
    let all_names: Vec<&String> = configs.keys().filter(|k| *k != "__default__").collect();
    let mut entries: Vec<(String, String)> = all_names
        .iter()
        .filter(|n| {
            // 根据 pending_mode 过滤传输层
            if let Some(args) = configs.get(n.as_str()) {
                match ui.pending_mode {
                    Some(MainMode::TcpMonitor) => {
                        args.main_mode.to_ascii_lowercase().contains("tcp")
                    }
                    Some(MainMode::RtuMonitor) => {
                        args.main_mode.to_ascii_lowercase().contains("rtu")
                    }
                    _ => true, // 未知模式不过滤
                }
            } else {
                true
            }
        })
        .map(|n| {
            let brief = configs.get(*n).map(profile_pick_brief).unwrap_or_default();
            ((*n).clone(), brief)
        })
        .collect();
    entries.sort_by(|a, b| a.0.cmp(&b.0));

    let mut items: Vec<Line> = Vec::new();
    for (i, (name, brief)) in entries.iter().enumerate() {
        if i == ui.menu_list_idx {
            items.push(Line::from(Span::styled(
                format!(" ○ {name}"),
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )));
            items.push(Line::from(Span::styled(
                format!("   {brief}"),
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::DIM),
            )));
        } else {
            items.push(Line::from(Span::styled(
                format!(" ○ {name}"),
                Style::default(),
            )));
            items.push(Line::from(Span::styled(
                format!("   {brief}"),
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::DIM),
            )));
        }
    }
    if entries.is_empty() {
        items.push(Line::from(Span::styled(
            t!("profile_settings.empty_list"),
            Style::default().fg(Color::DarkGray),
        )));
    }
    // 左侧列表 + 右侧预览
    let main =
        Layout::horizontal([Constraint::Percentage(55), Constraint::Percentage(45)]).split(vert[1]);

    let list_block = Block::default()
        .borders(Borders::ALL)
        .title(t!("run_ui.monitor_profile_pick_list_title"))
        .border_style(Style::default().fg(Color::Cyan));
    f.render_widget(Paragraph::new(items).block(list_block), main[0]);

    // 右侧：选中配置预览
    let right_content = if !entries.is_empty() && ui.menu_list_idx < entries.len() {
        let sel_name = &entries[ui.menu_list_idx].0;
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
                    t!(
                        "profile_pick.preview_mode",
                        mode = profile_monitor_mode_label(args)
                    )
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

    let help = t!("run_ui.monitor_profile_pick_help");
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            help,
            Style::default().fg(Color::DarkGray),
        )))
        .alignment(ratatui::layout::Alignment::Center),
        vert[2],
    );
}
pub(crate) fn render_csv_picker(f: &mut Frame<'_>, ui: &Ui) {
    let dialog_area = centered_rect(60, 70, f.area());
    f.render_widget(ratatui::widgets::Clear, dialog_area);

    let mut items: Vec<Line> = Vec::new();
    if ui.csv_files.is_empty() {
        items.push(Line::from(Span::styled(
            t!("run_ui.csv_no_files"),
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        for (i, path) in ui.csv_files.iter().enumerate() {
            let fname = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            let style = if i == ui.csv_pick_idx {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            };
            items.push(Line::from(Span::styled(format!(" {} ", fname), style)));
        }
    }

    let help_text = t!("run_ui.csv_pick_help");
    items.push(Line::from(Span::raw("")));
    items.push(Line::from(Span::styled(
        help_text,
        Style::default().fg(Color::DarkGray),
    )));

    let block = Block::default()
        .borders(Borders::ALL)
        .title(t!("run_ui.csv_pick_title"))
        .border_style(Style::default().fg(Color::Magenta));
    f.render_widget(Paragraph::new(items).block(block), dialog_area);
}
pub(crate) fn format_monitor_history(m: &MonitorStats, scroll: usize) -> String {
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
pub(crate) fn format_monitor_stats(m: &MonitorStats) -> String {
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

/// 将 RegChangePattern 转换为列表索引
pub(crate) fn pattern_index(p: &crate::RegChangePattern) -> usize {
    match p {
        crate::RegChangePattern::Random => 0,
        crate::RegChangePattern::UpDown => 1,
        crate::RegChangePattern::Sine => 2,
        crate::RegChangePattern::Square => 3,
        crate::RegChangePattern::Triangle => 4,
    }
}

/// 将模式列表索引转换为 RegChangePattern
pub(crate) fn index_to_pattern(idx: usize) -> crate::RegChangePattern {
    match idx {
        0 => crate::RegChangePattern::Random,
        1 => crate::RegChangePattern::UpDown,
        2 => crate::RegChangePattern::Sine,
        3 => crate::RegChangePattern::Square,
        _ => crate::RegChangePattern::Triangle,
    }
}

/// 将对话框中的模式选择写入 AppState
pub(crate) fn apply_pattern_dialog(ui: &mut Ui, s: &mut crate::AppState) {
    let pattern = index_to_pattern(ui.pattern_dialog_sel);
    let addr = ui.pattern_dialog_addr;
    match ui.pattern_dialog_reg_type {
        super::REG_VIEW_HOLDING => {
            while s.holding_change_patterns.len() <= addr {
                s.holding_change_patterns
                    .push(crate::RegChangePattern::Random);
                s.holding_pattern_freqs.push(1.0);
            }
            s.holding_change_patterns[addr] = pattern;
            s.holding_pattern_freqs[addr] = ui.pattern_dialog_freq;
        }
        super::REG_VIEW_INPUT => {
            while s.input_change_patterns.len() <= addr {
                s.input_change_patterns
                    .push(crate::RegChangePattern::Random);
                s.input_pattern_freqs.push(1.0);
            }
            s.input_change_patterns[addr] = pattern;
            s.input_pattern_freqs[addr] = ui.pattern_dialog_freq;
        }
        _ => {}
    }
}
