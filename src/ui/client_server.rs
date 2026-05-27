use super::{
    apply_pattern_dialog, centered_rect, edit_accepts_char, format_byte_panel,
    format_monitor_history, format_monitor_stats, format_protocol_analysis, parse_reg_format,
    pattern_index, reg_view_data, reg_view_len, render_csv_picker, render_monitor_profile_pick,
    search_match, set_status, wrapped_lines, Ui, REG_VIEW_COILS, REG_VIEW_DISCRETE,
    REG_VIEW_HOLDING, REG_VIEW_INPUT, UI_TIMEOUT,
};
use crate::{
    csv_log_append, csv_log_header, csv_log_path, export_registers_to_json, format_register_value,
    format_u16, list_csv_logs, load_csv_into_monitor, parse_u16_str, AppState, Args,
    ChangeDirection, RegCmd, BAR_HISTORY_SLOTS, MONITOR_LOG_DIR,
};
use anyhow::{anyhow, Context, Result};
use crossterm::{
    event::{Event, EventStream, KeyCode, KeyEvent, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use futures::StreamExt;
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Row, Table, TableState},
    Terminal,
};
use std::{collections::HashMap, io, sync::Arc};
use tokio::sync::{mpsc, oneshot, RwLock};
use tokio::time::{timeout, Duration};

pub async fn run_ui(
    state: Arc<RwLock<AppState>>,
    tx: mpsc::UnboundedSender<RegCmd>,
    args: Args,
    server_status: Arc<RwLock<Option<String>>>,
    config_path: String,
    profiles: Vec<String>,
    monitor_profile: Option<String>, // 菜单已选的配置名（监听模式自动选中）
) -> Result<()> {
    enable_raw_mode().context(t!("run_ui.enable_raw_mode"))?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen).context(t!("run_ui.enter_alt_screen"))?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).context(t!("run_ui.create_terminal"))?;

    let mut events = EventStream::new();
    let reg_format = parse_reg_format(&args.reg_format);
    let mut ui = Ui::new(reg_format, profiles);
    ui.config_path = config_path;
    ui.args = args.clone();

    // Monitor 模式默认开启监听面板
    if args.main_mode.contains("monitor") {
        ui.show_monitor = true;
    }

    // 如果菜单已选了配置，自动选中并启动监听任务
    if args.main_mode.contains("monitor") {
        if let Some(ref profile_name) = monitor_profile {
            if ui.profiles.contains(profile_name) {
                ui.monitor_selected_profile = Some(profile_name.clone());
                ui.monitor_picking = false;
                set_status(&mut ui, t!("run_ui.monitor_selected", name = profile_name));
                // 启动监听任务
                let cfg_path = ui.config_path.clone();
                let pname = profile_name.clone();
                let mon_state = Arc::clone(&state);
                tokio::spawn(async move {
                    let config_str = std::fs::read_to_string(&cfg_path).unwrap_or_default();
                    let configs: std::collections::HashMap<String, Args> =
                        toml::from_str(&config_str).unwrap_or_default();
                    if let Some(profile_args) = configs.get(&pname) {
                        let mut args = profile_args.clone();
                        let res = match args.main_mode.as_str() {
                            "rtu-monitor" => {
                                crate::modbus::run_modbus_monitor_rtu(args, mon_state).await
                            }
                            _ => {
                                args.main_mode = "tcp-monitor".to_string();
                                crate::modbus::run_modbus_monitor_tcp(args, mon_state).await
                            }
                        };
                        if let Err(e) = res {
                            eprintln!("监听任务失败: {}", e);
                        }
                    }
                });
            }
        }
    }

    let tick = Duration::from_millis(args.ui_tick_ms);
    let mut interval = tokio::time::interval(tick);

    let res: Result<()> = loop {
        tokio::select! {
                    _ = interval.tick() => {
                        let s = state.read().await;
                        // CSV logging: write any new frames to log file
                        if ui.monitor_logging {
                            if let Some(ref log_path) = ui.monitor_log_path {
                                let total = s.monitor.history.len();
                                if total > ui.last_logged_frames {
                                    for rec in s.monitor.history.iter().skip(ui.last_logged_frames) {
                                        let _ = csv_log_append(log_path, rec);
                                    }
                                    ui.last_logged_frames = total;
                                }
                            }
                        }
                        // 从 AppState 同步格式到 UI（允许程序化修改）
                        ui.reg_format = s.reg_format;
                        let server_err = server_status.read().await.clone();
                        let is_monitor_mode = args.main_mode.to_ascii_lowercase().contains("monitor");
                        terminal.draw(|f| {
                            let monitor_active = is_monitor_mode || ui.show_monitor;

                            // --- 预计算帮助文本（用于动态高度和值变化状态指示） ---
                            let mut help = if ui.edit_mode {
                                t!("run_ui.help_edit", buf = &ui.edit_buf).into_owned()
                            } else if s.stability_test_running {
                                t!("run_ui.help_stability").into_owned()
                            } else if is_monitor_mode {
                                let mut h = t!("run_ui.help_monitoring").into_owned();
                                if ui.monitor_logging {
                                    h.push_str(" | [LOG●]");
                                }
                                h
                            } else if monitor_active {
                                t!("run_ui.help_monitor").into_owned()
                            } else {
                                t!("run_ui.help_normal").into_owned()
                            };
                            // 追加启用值变化模拟的寄存器数量
                            let enabled_holding = s.holding_change_enabled.iter().filter(|&&e| e).count();
                            let enabled_input = s.input_change_enabled.iter().filter(|&&e| e).count();
                            let total = enabled_holding + enabled_input;
                            help.push_str(&format!(" | v:{}", total));

                            let term_width = f.area().width;
                            let panel_width = term_width.saturating_sub(2).max(1) as usize;
                            let help_lines = (wrapped_lines(&help, panel_width) + 2).max(3) as u16;

                            // 纯监听模式：仅显示监听面板；否则显示寄存器表 + 可选的监听覆盖层
                            let (monitor_constraint, keep) = if is_monitor_mode {
                                (Constraint::Min(3), false)
                            } else if monitor_active {
                                (Constraint::Length(12), true)
                            } else {
                                (Constraint::Length(0), false) // 不显示
                            };

                            let constraints: Vec<Constraint> = if is_monitor_mode {
                                vec![monitor_constraint, Constraint::Length(3), Constraint::Length(help_lines)]
                            } else if keep {
                                vec![Constraint::Min(3), monitor_constraint, Constraint::Length(3), Constraint::Length(help_lines)]
                            } else {
                                vec![Constraint::Min(5), Constraint::Length(3), Constraint::Length(help_lines)]
                            };

                            let areas = Layout::vertical(&constraints).split(f.area());
                            let mut area_idx = 0;

                            if is_monitor_mode {
                                // 纯监听模式：全屏监听面板
                                let monitor_area = areas[area_idx]; area_idx += 1;

                                if ui.monitor_picking || ui.monitor_selected_profile.is_none() {
                                    // 显示配置选择界面
                                    render_monitor_profile_pick(f, &ui, &ui.config_path);
                                } else {
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
                                }

                                // 协议分析对话框（覆盖在监听面板之上）
                                if ui.show_analysis_dialog {
                                    let total = s.monitor.history.len();
                                    if ui.analysis_idx < total {
                                        let rec = &s.monitor.history[ui.analysis_idx];
                                        let analysis_text = format_protocol_analysis(rec);
                                        let dialog_area = centered_rect(75, 80, f.area());
                                        let dialog = ratatui::widgets::Paragraph::new(analysis_text)
                                            .block(Block::default()
                                                .borders(Borders::ALL)
                                                .title(t!("run_ui.analysis_title"))
                                                .border_style(Style::default().fg(Color::Yellow))
                                            )
                                            .style(Style::default().fg(Color::Cyan).bg(Color::Black))
                                            .scroll((0, 0));
                                        f.render_widget(ratatui::widgets::Clear, dialog_area);
                                        f.render_widget(dialog, dialog_area);
                                    }
                                }

                                // CSV 文件选择对话框
                                if ui.csv_picking {
                                    render_csv_picker(f, &ui);
                                }
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
                            } else if ui.search_mode {
                                std::borrow::Cow::Owned(format!("/{}", ui.search_buf))
                            } else if let Some(m) = ui.status_msg.as_deref() {
                                std::borrow::Cow::Owned(m.to_string())
                            } else if ui.edit_mode {
                                if ui.edit_is_profile {
                                    t!("run_ui.edit_save_profile", buf = &ui.edit_buf)
                                } else if ui.edit_is_label {
                                    t!("run_ui.edit_label", buf = &ui.edit_buf)
                                } else {
                                    t!("run_ui.edit_value", base = ui.reg_format.short_label(), buf = &ui.edit_buf)
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
                                    let fmt = ui.reg_format.short_label();
                                let (sel_bytes, sel_words) = if ui.reg_view == REG_VIEW_INPUT {
                                    (s.input_swap_bytes.get(&ui.selected).copied().unwrap_or(false),
                                     s.input_swap_words.get(&ui.selected).copied().unwrap_or(false))
                                } else {
                                    (s.holding_swap_bytes.get(&ui.selected).copied().unwrap_or(false),
                                     s.holding_swap_words.get(&ui.selected).copied().unwrap_or(false))
                                };
                                let sw = if sel_bytes || sel_words {
                                    format!(" sw:{}{}", if sel_bytes {"B"} else {""}, if sel_words {"W"} else {""})
                                } else {
                                    String::new()
                                };
                                t!("run_ui.status_tcp", func = &fi.func_name, base = ui.reg_format.short_label(), fmt = fmt, sw = sw)
                                } else {
                                    let fmt = ui.reg_format.short_label();
                                    let (sel_bytes, sel_words) = if ui.reg_view == REG_VIEW_INPUT {
                                        (s.input_swap_bytes.get(&ui.selected).copied().unwrap_or(false),
                                         s.input_swap_words.get(&ui.selected).copied().unwrap_or(false))
                                    } else {
                                        (s.holding_swap_bytes.get(&ui.selected).copied().unwrap_or(false),
                                         s.holding_swap_words.get(&ui.selected).copied().unwrap_or(false))
                                    };
                                    let sw = if sel_bytes || sel_words {
                                        format!(" sw:{}{}", if sel_bytes {"B"} else {""}, if sel_words {"W"} else {""})
                                    } else {
                                        String::new()
                                    };
                                    t!("run_ui.status_rtu", func = &fi.func_name, base = ui.reg_format.short_label(), fmt = fmt, sw = sw)
                                }
                            } else {
                                let fmt = ui.reg_format.short_label();
                                let (sel_bytes, sel_words) = if ui.reg_view == REG_VIEW_INPUT {
                                    (s.input_swap_bytes.get(&ui.selected).copied().unwrap_or(false),
                                     s.input_swap_words.get(&ui.selected).copied().unwrap_or(false))
                                } else {
                                    (s.holding_swap_bytes.get(&ui.selected).copied().unwrap_or(false),
                                     s.holding_swap_words.get(&ui.selected).copied().unwrap_or(false))
                                };
                                let sw = if sel_bytes || sel_words {
                                    format!(" sw:{}{}", if sel_bytes {"B"} else {""}, if sel_words {"W"} else {""})
                                } else {
                                    String::new()
                                };
                                t!("run_ui.status_waiting", base = ui.reg_format.short_label(), fmt = fmt, sw = sw)
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

                            // --- 帮助栏（使用预计算的 help 文本，支持自动换行和动态高度） ---
                            f.render_widget(
                                ratatui::widgets::Paragraph::new(help.as_str())
                                    .wrap(ratatui::widgets::Wrap { trim: false })
                                    .block(Block::default().borders(Borders::ALL).title(t!("run_ui.help_title"))),
                                areas[help_index],
                            );

                            // --- 从设备扫描结果对话框 ---
                            if ui.show_scan_dialog {
                                if let Some(ref results) = s.slave_scan_result {
                                    let found: Vec<&(u8, Option<u16>)> = results.iter().filter(|(_, v)| v.is_some()).collect();
                                    let mut text = format!("{}\n\n", t!("run_ui.scan_found_slaves", count = found.len()));
                                    for (id, val) in &found {
                                        let v = format_u16(val.unwrap(), ui.reg_format);
                                        text.push_str(&format!("  Slave {}:  {}\n", id, v));
                                    }
                                    if found.is_empty() {
                                        text.push_str(&format!("  {}\n", t!("run_ui.scan_no_slaves")));
                                    }
                                    text.push_str(&format!("\n{}", t!("run_ui.scan_close_hint")));
                                    let dialog_area = centered_rect(50, 70, f.area());
                                    let dialog = ratatui::widgets::Paragraph::new(text)
                                        .block(
                                            Block::default()
                                                .borders(Borders::ALL)
                                                .title(t!("run_ui.scan_title"))
                                                .border_style(Style::default().fg(Color::Yellow)),
                                        )
                                        .style(Style::default().fg(Color::White).bg(Color::Black));
                                    f.render_widget(ratatui::widgets::Clear, dialog_area);
                                    f.render_widget(dialog, dialog_area);
                                }
                            }

                            // --- 配置信息弹窗 ---
                            if ui.show_profile_info {
                                render_profile_info(f, &ui);
                            }

                            // --- 寄存器变化模式配置对话框 ---
                            if ui.pattern_dialog_open {
                                render_pattern_dialog(f, &ui, &s);
                            }
                        })?;
                    }

                    maybe_ev = events.next() => {
                        let ev = match maybe_ev {
                            Some(Ok(ev)) => ev,
                            Some(Err(e)) => break Err(anyhow!(e).context("read event")),
                            None => continue,
                        };

                        if let Event::Key(KeyEvent { code, modifiers, kind, .. }) = ev {
                                if kind != crossterm::event::KeyEventKind::Press {
                                    continue;
                                }

                                if !ui.edit_mode
                                    && code == KeyCode::Char('c')
                                    && !modifiers.contains(KeyModifiers::CONTROL)
                                {
                                    *server_status.write().await = None;
                                    ui.status_msg = None;
                                    ui.show_change_bar = !ui.show_change_bar;
                                    if ui.show_change_bar {
                                        set_status(&mut ui, t!("run_ui.change_bar_on"));
                                    } else {
                                        set_status(&mut ui, t!("run_ui.change_bar_off"));
                                    }
                                }

                                if !ui.edit_mode
                                    && (code == KeyCode::Char('q')
                                        || (code == KeyCode::Char('c')
                                            && modifiers.contains(KeyModifiers::CONTROL)))
                                {
                                    break Ok(());
                                }

                                let is_monitor_mode = args.main_mode == "monitor";

                                // --- 显式关闭各对话框 ---
                                if ui.pattern_dialog_open {
                                    match code {
                                        KeyCode::Up | KeyCode::Char('k') if !ui.pattern_dialog_editing_freq => {
                                            ui.pattern_dialog_sel = ui.pattern_dialog_sel.saturating_sub(1);
                                        }
                                        KeyCode::Down | KeyCode::Char('j') if !ui.pattern_dialog_editing_freq => {
                                            if ui.pattern_dialog_sel < 4 {
                                                ui.pattern_dialog_sel += 1;
                                            }
                                        }
                                        KeyCode::Enter if !ui.pattern_dialog_editing_freq => {
                                            // 切换到频率编辑模式（仅对波形模式）
                                            if ui.pattern_dialog_sel >= 2 {
                                                ui.pattern_dialog_editing_freq = true;
                                                ui.pattern_dialog_freq_buf = format!("{:.2}", ui.pattern_dialog_freq);
                                            } else {
                                                // Random/UpDown 无需频率，直接确认
                                                apply_pattern_dialog(&mut ui, &mut *state.write().await);
                                                ui.pattern_dialog_open = false;
                                                set_status(&mut ui, "Pattern updated");
                                            }
                                        }
                                        KeyCode::Enter if ui.pattern_dialog_editing_freq => {
                                            // 确认频率编辑
                                            if let Ok(f) = ui.pattern_dialog_freq_buf.parse::<f64>() {
                                                let f = f.clamp(0.01, 1000.0);
                                                ui.pattern_dialog_freq = f;
                                            }
                                            ui.pattern_dialog_editing_freq = false;
                                            apply_pattern_dialog(&mut ui, &mut *state.write().await);
                                            ui.pattern_dialog_open = false;
                                            set_status(&mut ui, "Pattern updated");
                                        }
                                        KeyCode::Char(c) if ui.pattern_dialog_editing_freq && c.is_ascii_digit() || c == '.' => {
                                            ui.pattern_dialog_freq_buf.push(c);
                                        }
                                        KeyCode::Backspace if ui.pattern_dialog_editing_freq => {
                                            ui.pattern_dialog_freq_buf.pop();
                                        }
                                        KeyCode::Esc => {
                                            ui.pattern_dialog_open = false;
                                            ui.pattern_dialog_editing_freq = false;
                                            set_status(&mut ui, "Pattern config cancelled");
                                        }
                                        _ => {}
                                    }
                                } else if ui.edit_mode {
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
                profile_args.reg_combinations.clear();
                {
                    let s = state.read().await;
                    for (i, label) in s.holding_label.iter().enumerate() {
                        if !label.is_empty() {
                            profile_args.labels.insert(i.to_string(), label.clone());
                        }
                    }
                    // Save holding combinations
                    for (&addr, &fmt) in &s.holding_combinations {
                        profile_args.reg_combinations.insert(addr.to_string(), fmt.short_label().to_string());
                    }
                    // Save input combinations with "i:" prefix
                    for (&addr, &fmt) in &s.input_combinations {
                        profile_args.reg_combinations.insert(format!("i:{}", addr), fmt.short_label().to_string());
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
                let combos = {
                    let s = state.read().await;
                    match ui.reg_view {
                        REG_VIEW_HOLDING => s.holding_combinations.clone(),
                        REG_VIEW_INPUT => s.input_combinations.clone(),
                        _ => s.holding_combinations.clone(),
                    }
                };
                let fmt = combos
                    .get(&ui.selected)
                    .copied()
                    .unwrap_or(ui.reg_format);
                let (swap_bytes, swap_words) = {
                    let s = state.read().await;
                    if ui.reg_view == REG_VIEW_INPUT {
                        (
                            s.input_swap_bytes.get(&ui.selected).copied().unwrap_or(false),
                            s.input_swap_words.get(&ui.selected).copied().unwrap_or(false),
                        )
                    } else {
                        (
                            s.holding_swap_bytes.get(&ui.selected).copied().unwrap_or(false),
                            s.holding_swap_words.get(&ui.selected).copied().unwrap_or(false),
                        )
                    }
                };
                match crate::parse_register_value(&ui.edit_buf, fmt, swap_bytes, swap_words) {
                    Ok(reg_values) => {
                        if reg_values.len() == 1 {
                            let (resp_tx,_) = oneshot::channel();
                            let _ = tx.send(RegCmd::WriteSingleHolding {
                                addr: ui.selected,
                                value: reg_values[0],
                                resp: resp_tx,
                            });
                        } else {
                            let (resp_tx,_) = oneshot::channel();
                            let _ = tx.send(RegCmd::WriteMultipleHolding {
                                addr: ui.selected,
                                values: reg_values,
                                resp: resp_tx,
                            });
                        }
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
                                                match parse_u16_str(&ui.edit_buf, ui.reg_format) {
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
                                            ui.status_msg = None;
                                                ui.edit_buf.push(ch);
                                        }

                                        _ => {}
                                    }
                                } else if ui.show_analysis_dialog {
                                    // 对话框已打开 → 按 Esc 关闭
                                    match code {
                                        KeyCode::Esc | KeyCode::Enter => {
                                            ui.show_analysis_dialog = false;
                                            ui.monitor_focus_history = true;
                                            set_status(&mut ui, t!("run_ui.analysis_closed"));
                                        }
                                        _ => {}
                                    }
                                } else if ui.show_scan_dialog {
                                    // 扫描结果对话框 → 按任意键关闭
                                    match code {
                                        KeyCode::Esc | KeyCode::Enter | KeyCode::Char('l') => {
                                            ui.show_scan_dialog = false;
                                            let mut s = state.write().await;
                                            s.slave_scan_result = None;
                                            set_status(&mut ui, t!("run_ui.scan_closed"));
                                        }
                                        _ => {}
                                    }
                                } else if ui.show_profile_info {
                                    // 配置信息弹窗 → 按任意键关闭
                                    match code {
                                        KeyCode::Esc | KeyCode::Char('i') => {
                                            ui.show_profile_info = false;
                                        }
                                        _ => {}
                                    }
                                } else if ui.goto_mode {
                                    // 跳转地址输入模式
                                    match code {
                                        KeyCode::Char(ch) if ch.is_ascii_digit() => {
                                            ui.goto_buf.push(ch);
                                        }
                                        KeyCode::Enter => {
                                            let addr: usize = ui.goto_buf.parse().unwrap_or(0);
                                            let view = ui.reg_view;
                                            let max = if view == REG_VIEW_HOLDING {
                                                state.read().await.holding.len()
                                            } else if view == REG_VIEW_COILS {
                                                state.read().await.coils.len()
                                            } else if view == REG_VIEW_DISCRETE {
                                                state.read().await.discrete.len()
                                            } else {
                                                state.read().await.input_registers.len()
                                            };
                                            if addr < max {
                                                ui.selected = addr;
                                                ui.status_msg = Some(format!("跳转到地址 {}", addr));
                                            } else {
                                                set_status(&mut ui, format!("地址 {} 超出范围 (0-{})", addr, max.saturating_sub(1)));
                                            }
                                            ui.goto_mode = false;
                                            ui.goto_buf.clear();
                                        }
                                        KeyCode::Backspace => {
                                            ui.goto_buf.pop();
                                        }
                                        KeyCode::Esc => {
                                            ui.goto_mode = false;
                                            ui.goto_buf.clear();
                                            ui.status_msg = None;
                                        }
                                        _ => {}
                                    }
                                } else if ui.search_mode {
                                    match code {
                                        KeyCode::Char(ch) if ch.is_ascii_graphic() || ch == ' ' => {
                                            ui.search_buf.push(ch);
                                        }
                                        KeyCode::Backspace => {
                                            ui.search_buf.pop();
                                        }
                                        KeyCode::Enter | KeyCode::Esc => {
                                            ui.search_mode = false;
                                            if ui.search_buf.is_empty() {
                                                ui.status_msg = None;
                                            } else {
                                                let msg = format!("搜索: {}", ui.search_buf);
                                                set_status(&mut ui, msg);
                                            }
                                        }
                                        _ => {}
                                    }
                                } else if ui.csv_picking {
                                    match code {
                                        KeyCode::Up | KeyCode::Char('k') => {
                                            ui.csv_pick_idx = ui.csv_pick_idx.saturating_sub(1);
                                        }
                                        KeyCode::Down | KeyCode::Char('j') => {
                                            if !ui.csv_files.is_empty() {
                                                ui.csv_pick_idx = (ui.csv_pick_idx + 1).min(ui.csv_files.len().saturating_sub(1));
                                            }
                                        }
                                        KeyCode::Enter => {
                                            if ui.csv_pick_idx < ui.csv_files.len() {
                                                let path = ui.csv_files[ui.csv_pick_idx].clone();
                                                match load_csv_into_monitor(&path) {
                                                    Ok(stats) => {
                                                        let mut s = state.write().await;
                                                        s.monitor = stats;
                                                        ui.csv_picking = false;
                                                        ui.csv_replay_active = true;
                                                        ui.monitor_scroll = 0;
                                                        let fname = path.file_name().unwrap_or_default().to_string_lossy().to_string();
                                                        set_status(&mut ui, t!("run_ui.csv_loaded", file = fname));
                                                    }
                                                    Err(e) => {
                                                        set_status(&mut ui, format!("CSV load error: {}", e));
                                                    }
                                                }
                                            }
                                        }
                                        KeyCode::Esc => {
                                            ui.csv_picking = false;
                                        }
                                        _ => {}
                                    }
                                } else {
                                    match code {
                                        KeyCode::Enter => {
                                            if is_monitor_mode && ui.monitor_picking {
                                                let idx = ui.menu_list_idx;
                                                if idx < ui.profiles.len() {
                                                    let name = ui.profiles[idx].clone();
                                                    ui.monitor_selected_profile = Some(name.clone());
                                                    ui.monitor_picking = false;
                                                    set_status(&mut ui, t!("run_ui.monitor_selected", name = &name));
                                                    // 异步加载配置并启动监听任务
                                                    let cfg_path = ui.config_path.clone();
                                                    let pname = name.clone();
                                                    let mon_state = Arc::clone(&state);
                                                    tokio::spawn(async move {
                                                        let config_str = std::fs::read_to_string(&cfg_path).unwrap_or_default();
                                                        let configs: std::collections::HashMap<String, Args> = toml::from_str(&config_str).unwrap_or_default();
                                                        if let Some(profile_args) = configs.get(&pname) {
                                                            let mut args = profile_args.clone();
                                                            // 根据配置的 main_mode 选择对应的监听函数
                                                            let res = match args.main_mode.as_str() {
                                                                "rtu-monitor" => {
                                                                    crate::modbus::run_modbus_monitor_rtu(args, mon_state).await
                                                                }
                                                                _ => {
                                                                    args.main_mode = "tcp-monitor".to_string();
                                                                    crate::modbus::run_modbus_monitor_tcp(args, mon_state).await
                                                                }
                                                            };
                                                            if let Err(e) = res {
                                                                eprintln!("监听任务失败: {}", e);
                                                            }
                                                        }
                                                    });
                                                }
                                            } else if is_monitor_mode && ui.monitor_focus_history && !ui.monitor_picking {
                                                // 在历史面板按 Enter → 打开协议分析对话框
                                                let total = state.read().await.monitor.history.len();
                                                if total > 0 {
                                                    let idx = total.saturating_sub(1).saturating_sub(ui.monitor_scroll);
                                                    if idx < total {
                                                        ui.analysis_idx = idx;
                                                        ui.show_analysis_dialog = true;
                                                        set_status(&mut ui, t!("run_ui.analysis_opened"));
                                                    }
                                                }
                                            }
                                        }
                                        KeyCode::PageDown => {
                                            let len = state.read().await.holding.len();
                                            ui.selected = len.saturating_sub(1);
                                        }
                                        KeyCode::PageUp => {
                                            ui.selected = 0;
                                        }
                                        KeyCode::Home => {
                                            ui.selected = 0;
                                            ui.scroll = 0;
                                            set_status(&mut ui, t!("run_ui.home"));
                                        }
                                        KeyCode::End => {
                                            let len = if ui.reg_view == REG_VIEW_HOLDING {
                                                state.read().await.holding.len()
                                            } else if ui.reg_view == REG_VIEW_COILS {
                                                state.read().await.coils.len()
                                            } else if ui.reg_view == REG_VIEW_DISCRETE {
                                                state.read().await.discrete.len()
                                            } else {
                                                state.read().await.input_registers.len()
                                            };
                                            if len > 0 {
                                                ui.selected = len - 1;
                                                set_status(&mut ui, t!("run_ui.end"));
                                            }
                                        }

                                        KeyCode::Char('k') | KeyCode::Up => {
                                            if is_monitor_mode && ui.monitor_picking {
                                                ui.menu_list_idx = ui.menu_list_idx.saturating_sub(1);
                                            } else if is_monitor_mode || (ui.show_monitor && ui.monitor_focus_history) {
                                                ui.monitor_scroll = ui.monitor_scroll.saturating_sub(1);
                                            } else if ui.search_mode && !ui.search_buf.is_empty() {
                                                // 搜索模式下：跳到上一个匹配的地址
                                                let s = state.read().await;
                                                let search_lower = ui.search_buf.to_lowercase();
                                                let (items, labels) = reg_view_data(&s, ui.reg_view);
                                                if ui.selected > 0 {
                                                    let found = (0..ui.selected).rev().find(|&i| {
                                                        search_match(i, &search_lower, items, labels)
                                                    });
                                                    if let Some(idx) = found {
                                                        ui.selected = idx;
                                                    }
                                                }
                                                drop(s);
                                            } else {
                                                let s = state.read().await;
                                                let combinations = match ui.reg_view {
                                                    REG_VIEW_HOLDING => &s.holding_combinations,
                                                    REG_VIEW_INPUT => &s.input_combinations,
                                                    _ => &s.holding_combinations,
                                                };
                                                if let Some(addr) = prev_visible_reg(ui.selected, combinations) {
                                                    ui.selected = addr;
                                                }
                                                drop(s);
                                            }
                                        }
                                        KeyCode::Char('j') | KeyCode::Down => {
                                            if is_monitor_mode && ui.monitor_picking {
                                                let len = ui.profiles.len();
                                                ui.menu_list_idx = (ui.menu_list_idx + 1).min(len.saturating_sub(1));
                                            } else if is_monitor_mode || (ui.show_monitor && ui.monitor_focus_history) {
                                                let len = state.read().await.monitor.history.len();
                                                if ui.monitor_scroll + 1 < len.saturating_sub(8) {
                                                    ui.monitor_scroll += 1;
                                                }
                                            } else if ui.search_mode && !ui.search_buf.is_empty() {
                                                // 搜索模式下：跳到下一个匹配的地址
                                                let s = state.read().await;
                                                let search_lower = ui.search_buf.to_lowercase();
                                                let (items, labels) = reg_view_data(&s, ui.reg_view);
                                                let max = items.len();
                                                let found = (ui.selected + 1..max).find(|&i| {
                                                    search_match(i, &search_lower, items, labels)
                                                });
                                                if let Some(idx) = found {
                                                    ui.selected = idx;
                                                }
                                                drop(s);
                                            } else {
                                                let s = state.read().await;
                                                let max = reg_view_len(&s, ui.reg_view);
                                                let combinations = match ui.reg_view {
                                                    REG_VIEW_HOLDING => &s.holding_combinations,
                                                    REG_VIEW_INPUT => &s.input_combinations,
                                                    _ => &s.holding_combinations,
                                                };
                                                if let Some(addr) = next_visible_reg(ui.selected, max, combinations) {
                                                    ui.selected = addr;
                                                }
                                                drop(s);
                                            }
                                        }
                                        // I (Shift+I): 显示/关闭当前配置信息弹窗
                                        KeyCode::Char('I') => {
                                            if !ui.edit_mode {
                                                ui.show_profile_info = !ui.show_profile_info;
                                            }
                                        }

                                        // u: 设置类型为 Uint（保持当前位宽）
                                        KeyCode::Char('u') => {
                                            if !ui.edit_mode && !is_monitor_mode {
                                                let mut s = state.write().await;
                                                if ui.reg_view == REG_VIEW_HOLDING || ui.reg_view == REG_VIEW_INPUT {
                                                    let addr = ui.selected;
                                                    let total_regs = match ui.reg_view {
                                                        REG_VIEW_HOLDING => s.holding.len(),
                                                        REG_VIEW_INPUT => s.input_registers.len(),
                                                        _ => unreachable!(),
                                                    };
                                                    if addr < total_regs {
                                                        let combinations = match ui.reg_view {
                                                            REG_VIEW_HOLDING => &mut s.holding_combinations,
                                                            REG_VIEW_INPUT => &mut s.input_combinations,
                                                            _ => unreachable!(),
                                                        };
                                                        let current_fmt = combinations
                                                            .get(&addr)
                                                            .copied()
                                                            .unwrap_or(ui.reg_format);
                                                        let new_fmt = current_fmt.to_uint();
                                                        combinations.insert(addr, new_fmt);
                                                        let msg = format!("Format: {} @ reg {}", new_fmt.short_label(), addr);
                                                        set_status(&mut ui, msg);
                                                        drop(s);
                                                    } else {
                                                        drop(s);
                                                    }
                                                } else {
                                                    drop(s);
                                                }
                                            }
                                        }
                                        // i: 设置类型为 Int（保持当前位宽）
                                        KeyCode::Char('i') => {
                                            if !ui.edit_mode && !is_monitor_mode {
                                                let mut s = state.write().await;
                                                if ui.reg_view == REG_VIEW_HOLDING || ui.reg_view == REG_VIEW_INPUT {
                                                    let addr = ui.selected;
                                                    let total_regs = match ui.reg_view {
                                                        REG_VIEW_HOLDING => s.holding.len(),
                                                        REG_VIEW_INPUT => s.input_registers.len(),
                                                        _ => unreachable!(),
                                                    };
                                                    if addr < total_regs {
                                                        let combinations = match ui.reg_view {
                                                            REG_VIEW_HOLDING => &mut s.holding_combinations,
                                                            REG_VIEW_INPUT => &mut s.input_combinations,
                                                            _ => unreachable!(),
                                                        };
                                                        let current_fmt = combinations
                                                            .get(&addr)
                                                            .copied()
                                                            .unwrap_or(ui.reg_format);
                                                        let new_fmt = current_fmt.to_int();
                                                        combinations.insert(addr, new_fmt);
                                                        let msg = format!("Format: {} @ reg {}", new_fmt.short_label(), addr);
                                                        set_status(&mut ui, msg);
                                                        drop(s);
                                                    } else {
                                                        drop(s);
                                                    }
                                                } else {
                                                    drop(s);
                                                }
                                            }
                                        }
                                        // f / F: 循环数据类型（Uint→Int→Float→Hex→Binary→Uint），绝不改变位宽
                                        KeyCode::Char('f') | KeyCode::Char('F') => {
                                            if !ui.edit_mode && !is_monitor_mode {
                                                let mut s = state.write().await;
                                                if ui.reg_view == REG_VIEW_HOLDING || ui.reg_view == REG_VIEW_INPUT {
                                                    let addr = ui.selected;
                                                    let total_regs = match ui.reg_view {
                                                        REG_VIEW_HOLDING => s.holding.len(),
                                                        REG_VIEW_INPUT => s.input_registers.len(),
                                                        _ => unreachable!(),
                                                    };
                                                    if addr < total_regs {
                                                        let new_fmt = {
                                                            let combinations = match ui.reg_view {
                                                                REG_VIEW_HOLDING => &mut s.holding_combinations,
                                                                REG_VIEW_INPUT => &mut s.input_combinations,
                                                                _ => unreachable!(),
                                                            };
                                                            let current_fmt = combinations
                                                                .get(&addr)
                                                                .copied()
                                                                .unwrap_or(ui.reg_format);
                                                            let old_needed = current_fmt.regs_needed();
                                                            combinations.retain(|&k, _| k < addr || k >= addr + old_needed);
                                                            current_fmt.next_type()
                                                        };
                                                        let new_needed = new_fmt.regs_needed();
                                                        if new_needed > 1 && addr + new_needed <= total_regs {
                                                            let change_enabled = match ui.reg_view {
                                                                REG_VIEW_HOLDING => &mut s.holding_change_enabled,
                                                                REG_VIEW_INPUT => &mut s.input_change_enabled,
                                                                _ => unreachable!(),
                                                            };
                                                            for i in (addr + 1)..(addr + new_needed).min(change_enabled.len()) {
                                                                change_enabled[i] = false;
                                                            }
                                                        }
                                                        {
                                                            let combinations = match ui.reg_view {
                                                                REG_VIEW_HOLDING => &mut s.holding_combinations,
                                                                REG_VIEW_INPUT => &mut s.input_combinations,
                                                                _ => unreachable!(),
                                                            };
                                                            combinations.insert(addr, new_fmt);
                                                        }
                                                        let msg = format!("Format: {} @ reg {}", new_fmt.short_label(), addr);
                                                        set_status(&mut ui, msg);
                                                        drop(s);
                                                    } else {
                                                        drop(s);
                                                    }
                                                } else {
                                                    drop(s);
                                                }
                                            }
                                        }
                                        // h: 设置类型为 Hex，保持当前位宽
                                        KeyCode::Char('h') => {
                                            if !ui.edit_mode && !is_monitor_mode {
                                                let mut s = state.write().await;
                                                if ui.reg_view == REG_VIEW_HOLDING || ui.reg_view == REG_VIEW_INPUT {
                                                    let addr = ui.selected;
                                                    let total_regs = match ui.reg_view {
                                                        REG_VIEW_HOLDING => s.holding.len(),
                                                        REG_VIEW_INPUT => s.input_registers.len(),
                                                        _ => unreachable!(),
                                                    };
                                                    if addr < total_regs {
                                                        let new_fmt = {
                                                            let combinations = match ui.reg_view {
                                                                REG_VIEW_HOLDING => &mut s.holding_combinations,
                                                                REG_VIEW_INPUT => &mut s.input_combinations,
                                                                _ => unreachable!(),
                                                            };
                                                            let current_fmt = combinations
                                                                .get(&addr)
                                                                .copied()
                                                                .unwrap_or(ui.reg_format);
                                                            let old_needed = current_fmt.regs_needed();
                                                            combinations.retain(|&k, _| k < addr || k >= addr + old_needed);
                                                            current_fmt.to_hex()
                                                        };
                                                        let new_needed = new_fmt.regs_needed();
                                                        if new_needed > 1 && addr + new_needed <= total_regs {
                                                            let change_enabled = match ui.reg_view {
                                                                REG_VIEW_HOLDING => &mut s.holding_change_enabled,
                                                                REG_VIEW_INPUT => &mut s.input_change_enabled,
                                                                _ => unreachable!(),
                                                            };
                                                            for i in (addr + 1)..(addr + new_needed).min(change_enabled.len()) {
                                                                change_enabled[i] = false;
                                                            }
                                                        }
                                                        {
                                                            let combinations = match ui.reg_view {
                                                                REG_VIEW_HOLDING => &mut s.holding_combinations,
                                                                REG_VIEW_INPUT => &mut s.input_combinations,
                                                                _ => unreachable!(),
                                                            };
                                                            combinations.insert(addr, new_fmt);
                                                        }
                                                        let msg = format!("Format: {} @ reg {}", new_fmt.short_label(), addr);
                                                        set_status(&mut ui, msg);
                                                        drop(s);
                                                    } else {
                                                        drop(s);
                                                    }
                                                } else {
                                                    drop(s);
                                                }
                                            }
                                        }
                                        // g: 切换选中地址的数据位宽 16→32→64→128→16 (保持格式类型)
                                        // 同时更新选中寄存器的组合配置，使其正确合并/隐藏相邻寄存器
                                        KeyCode::Char('g') => {
                                            if !ui.edit_mode {
                                                let mut s = state.write().await;
                                                // 对 holding/input 寄存器视图，更新组合配置实现寄存器合并
                                                if ui.reg_view == REG_VIEW_HOLDING || ui.reg_view == REG_VIEW_INPUT {
                                                    let addr = ui.selected;
                                                    let total_regs = match ui.reg_view {
                                                        REG_VIEW_HOLDING => s.holding.len(),
                                                        REG_VIEW_INPUT => s.input_registers.len(),
                                                        _ => unreachable!(),
                                                    };
                                                    if addr < total_regs {
                                                        let combinations = match ui.reg_view {
                                                            REG_VIEW_HOLDING => &mut s.holding_combinations,
                                                            REG_VIEW_INPUT => &mut s.input_combinations,
                                                            _ => unreachable!(),
                                                        };
                                                        // 当前格式：优先从组合读，没有则用全局默认
                                                        let current_fmt = combinations
                                                            .get(&addr)
                                                            .copied()
                                                            .unwrap_or(ui.reg_format);
                                                        let new_fmt = current_fmt.next_width();
                                                        let new_needed = new_fmt.regs_needed();
                                                        // 移除与选中寄存器范围重叠的所有旧组合
                                                        combinations.retain(|&k, _| k < addr || k >= addr + new_needed);
                                                        if new_needed > 1 && addr + new_needed <= total_regs {
                                                            // 插入新组合：选中寄存器作为 primary
                                                            combinations.insert(addr, new_fmt);
                                                            // 禁用次级寄存器的变化追踪
                                                            let change_enabled = match ui.reg_view {
                                                                REG_VIEW_HOLDING => &mut s.holding_change_enabled,
                                                                REG_VIEW_INPUT => &mut s.input_change_enabled,
                                                                _ => unreachable!(),
                                                            };
                                                            for i in (addr + 1)..(addr + new_needed).min(change_enabled.len()) {
                                                                change_enabled[i] = false;
                                                            }
                                                            // 清理次级寄存器的变化历史
                                                            for i in (addr + 1)..(addr + new_needed) {
                                                                if i < s.reg_just_changed.len() {
                                                                    s.reg_just_changed[i] = false;
                                                                }
                                                                if i < s.reg_change_direction.len() {
                                                                    s.reg_change_direction[i] = ChangeDirection::Up;
                                                                }
                                                            }
                                                            let w = new_fmt.short_label();
                                                            let msg = format!(
                                                                "Width: {} bit | Reg {} merged with next {} reg(s)",
                                                                &w[1..],
                                                                addr,
                                                                new_needed - 1
                                                            );
                                                            set_status(&mut ui, msg);
                                                        } else {
                                                            // 回到 16 位，移除该地址的组合
                                                            combinations.remove(&addr);
                                                            let msg = format!("Width: 16 bit | Reg {} unmerged", addr);
                                                            set_status(&mut ui, msg);
                                                        }
                                                    }
                                                }
                                                drop(s);
                                            }
                                        }
                                        // G (Shift+G): 切换选中地址的数据位宽反向 16←32←64←128←16
                                        // 同时更新选中寄存器的组合配置，使其正确合并/隐藏相邻寄存器
                                        KeyCode::Char('G') => {
                                            if !ui.edit_mode {
                                                let mut s = state.write().await;
                                                // 对 holding/input 寄存器视图，更新组合配置实现寄存器合并
                                                if ui.reg_view == REG_VIEW_HOLDING || ui.reg_view == REG_VIEW_INPUT {
                                                    let addr = ui.selected;
                                                    let total_regs = match ui.reg_view {
                                                        REG_VIEW_HOLDING => s.holding.len(),
                                                        REG_VIEW_INPUT => s.input_registers.len(),
                                                        _ => unreachable!(),
                                                    };
                                                    if addr < total_regs {
                                                        let combinations = match ui.reg_view {
                                                            REG_VIEW_HOLDING => &mut s.holding_combinations,
                                                            REG_VIEW_INPUT => &mut s.input_combinations,
                                                            _ => unreachable!(),
                                                        };
                                                        let current_fmt = combinations
                                                            .get(&addr)
                                                            .copied()
                                                            .unwrap_or(ui.reg_format);
                                                        let new_fmt = current_fmt.prev_width();
                                                        let new_needed = new_fmt.regs_needed();
                                                        // 移除与选中寄存器范围重叠的所有旧组合
                                                        combinations.retain(|&k, _| k < addr || k >= addr + new_needed);
                                                        if new_needed > 1 && addr + new_needed <= total_regs {
                                                            // 插入新组合：选中寄存器作为 primary
                                                            combinations.insert(addr, new_fmt);
                                                            // 禁用次级寄存器的变化追踪
                                                            let change_enabled = match ui.reg_view {
                                                                REG_VIEW_HOLDING => &mut s.holding_change_enabled,
                                                                REG_VIEW_INPUT => &mut s.input_change_enabled,
                                                                _ => unreachable!(),
                                                            };
                                                            for i in (addr + 1)..(addr + new_needed).min(change_enabled.len()) {
                                                                change_enabled[i] = false;
                                                            }
                                                            // 清理次级寄存器的变化历史
                                                            for i in (addr + 1)..(addr + new_needed) {
                                                                if i < s.reg_just_changed.len() {
                                                                    s.reg_just_changed[i] = false;
                                                                }
                                                                if i < s.reg_change_direction.len() {
                                                                    s.reg_change_direction[i] = ChangeDirection::Up;
                                                                }
                                                            }
                                                            let w = new_fmt.short_label();
                                                            let msg = format!(
                                                                "Width: {} bit | Reg {} merged with next {} reg(s)",
                                                                &w[1..],
                                                                addr,
                                                                new_needed - 1
                                                            );
                                                            set_status(&mut ui, msg);
                                                        } else {
                                                            // 回到 16 位，移除该地址的组合
                                                            combinations.remove(&addr);
                                                            let msg = format!("Width: 16 bit | Reg {} unmerged", addr);
                                                            set_status(&mut ui, msg);
                                                        }
                                                    }
                                                }
                                                drop(s);
                                            }
                                        }
                                        // w: 切换当前选中地址的字节序交换
                                        KeyCode::Char('w') => {
                                            if !is_monitor_mode && !ui.edit_mode {
                                                let mut s = state.write().await;
                                                let map = if ui.reg_view == REG_VIEW_INPUT {
                                                    &mut s.input_swap_bytes
                                                } else {
                                                    &mut s.holding_swap_bytes
                                                };
                                                let new_val = !map.get(&ui.selected).copied().unwrap_or(false);
                                                if new_val {
                                                    map.insert(ui.selected, true);
                                                } else {
                                                    map.remove(&ui.selected);
                                                }
                                                let sel = ui.selected;
                                                drop(s);
                                                if new_val { set_status(&mut ui, format!("Byte swap: ON for addr {}", sel)); }
                                                else { set_status(&mut ui, format!("Byte swap: OFF for addr {}", sel)); }
                                            }
                                        }
                                        // W (Shift+W): 切换当前选中地址的字序交换
                                        KeyCode::Char('W') => {
                                            if !is_monitor_mode && !ui.edit_mode {
                                                let mut s = state.write().await;
                                                let map = if ui.reg_view == REG_VIEW_INPUT {
                                                    &mut s.input_swap_words
                                                } else {
                                                    &mut s.holding_swap_words
                                                };
                                                let new_val = !map.get(&ui.selected).copied().unwrap_or(false);
                                                if new_val {
                                                    map.insert(ui.selected, true);
                                                } else {
                                                    map.remove(&ui.selected);
                                                }
                                                let sel = ui.selected;
                                                drop(s);
                                                if new_val { set_status(&mut ui, format!("Word swap: ON for addr {}", sel)); }
                                                else { set_status(&mut ui, format!("Word swap: OFF for addr {}", sel)); }
                                            }
                                        }
                                        // E (Shift+E): 导出当前寄存器到 JSON 文件
                                        KeyCode::Char('E') => {
                                            if !is_monitor_mode && !ui.edit_mode {
                                                let s = state.read().await;
                                                match export_registers_to_json(ui.reg_format, &s) {
                                                    Ok((filename, json)) => {
                                                        drop(s);
                                                        match std::fs::write(&filename, &json) {
                                                            Ok(_) => set_status(&mut ui, format!("Exported: {}", filename)),
                                                            Err(e) => set_status(&mut ui, format!("Export error: {}", e)),
                                                        }
                                                    }
                                                    Err(e) => {
                                                        drop(s);
                                                        set_status(&mut ui, format!("Export error: {}", e));
                                                    }
                                                }
                                            }
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
                                            let items = match ui.reg_view {
                                                REG_VIEW_HOLDING => &s.holding,
                                                REG_VIEW_INPUT => &s.input_registers,
                                                _ => &s.holding,
                                            };
                                            if ui.selected < items.len() {
                                                ui.edit_mode = true;
                                                ui.edit_is_label = false;
                                                ui.edit_is_profile = false;
                                                let combos = match ui.reg_view {
                                                    REG_VIEW_HOLDING => &s.holding_combinations,
                                                    REG_VIEW_INPUT => &s.input_combinations,
                                                    _ => &s.holding_combinations,
                                                };
                                                let fmt = combos
                                                    .get(&ui.selected)
                                                    .copied()
                                                    .unwrap_or(ui.reg_format);
                                                let (sel_swap_bytes, sel_swap_words) =
                                                    if ui.reg_view == REG_VIEW_INPUT {
                                                        (
                                                            s.input_swap_bytes
                                                                .get(&ui.selected)
                                                                .copied()
                                                                .unwrap_or(false),
                                                            s.input_swap_words
                                                                .get(&ui.selected)
                                                                .copied()
                                                                .unwrap_or(false),
                                                        )
                                                    } else {
                                                        (
                                                            s.holding_swap_bytes
                                                                .get(&ui.selected)
                                                                .copied()
                                                                .unwrap_or(false),
                                                            s.holding_swap_words
                                                                .get(&ui.selected)
                                                                .copied()
                                                                .unwrap_or(false),
                                                        )
                                                    };
                                                ui.edit_buf = crate::format_register_value(
                                                    items,
                                                    ui.selected,
                                                    fmt,
                                                    sel_swap_bytes,
                                                    sel_swap_words,
                                                );
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
                                        KeyCode::Char('P') => {
                                            if is_monitor_mode && !ui.profiles.is_empty() {
                                                ui.monitor_picking = !ui.monitor_picking;
                                                if ui.monitor_picking {
                                                    set_status(&mut ui, t!("run_ui.monitor_picking"));
                                                }
                                            }
                                        }
                                        // L: Toggle CSV logging
                                        KeyCode::Char('L') => {
                                            if is_monitor_mode || ui.show_monitor {
                                                ui.monitor_logging = !ui.monitor_logging;
                                                if ui.monitor_logging {
                                                    // Create monitor directory and new log file
                                                    let dir = std::path::Path::new(MONITOR_LOG_DIR);
                                                    if !dir.exists() {
                                                        let _ = std::fs::create_dir_all(dir);
                                                    }
                                                    let path = csv_log_path(&args.main_mode, args.tcp_port, &args.device);
                                                    if let Err(e) = csv_log_header(&path) {
                                                        set_status(&mut ui, format!("CSV log error: {}", e));
                                                        ui.monitor_logging = false;
                                                    } else {
                                                        ui.monitor_log_path = Some(path.clone());
                                                        set_status(&mut ui, t!("run_ui.logging_started", path = path.display().to_string()));
                                                    }
                                                } else {
                                                    ui.monitor_log_path = None;
                                                    set_status(&mut ui, t!("run_ui.logging_stopped"));
                                                }
                                            }
                                        }
                                        // O: Open CSV file picker for replay
                                        KeyCode::Char('O') => {
                                            if is_monitor_mode {
                                                ui.csv_files = list_csv_logs();
                                                ui.csv_pick_idx = 0;
                                                ui.csv_picking = !ui.csv_picking;
                                                if ui.csv_picking {
                                                    set_status(&mut ui, t!("run_ui.csv_pick_opened"));
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
                                        KeyCode::Char('v') => {
                                            // 切换当前选中寄存器的值变化模拟开关
                                            if !is_monitor_mode && !ui.edit_mode && !ui.show_monitor {
                                                let reg_type = ui.reg_view;
                                                let addr = ui.selected;
                                                let reg_len = if reg_type == REG_VIEW_HOLDING {
                                                    state.read().await.holding.len()
                                                } else if reg_type == REG_VIEW_INPUT {
                                                    state.read().await.input_registers.len()
                                                } else {
                                                    set_status(&mut ui, t!("run_ui.value_change_unsupported"));
                                                    continue;
                                                };
                                                let mut s = state.write().await;
                                                let enabled = if reg_type == REG_VIEW_HOLDING {
                                                    &mut s.holding_change_enabled
                                                } else {
                                                    &mut s.input_change_enabled
                                                };
                                                if addr < reg_len {
                                                    while enabled.len() <= addr {
                                                        enabled.push(false);
                                                    }
                                                    enabled[addr] = !enabled[addr];
                                                    let status = if enabled[addr] {
                                                        t!("run_ui.value_change_on")
                                                    } else {
                                                        t!("run_ui.value_change_off")
                                                    };
                                                    let reg_name = if reg_type == REG_VIEW_HOLDING { "Holding" } else { "Input" };
                                                    drop(s);
                                                    set_status(&mut ui, format!("{}[{}] {}", reg_name, addr, status));
                                                } else {
                                                    drop(s);
                                                }
                                            }
                                        }
                                        // 寄存器视图切换：R 循环切换 4 种寄存器类型
                                        KeyCode::Char('R') => {
                                            if !is_monitor_mode {
                                                let names = ["Holding", "Coils", "Discrete", "Input"];
                                                let idx = (ui.reg_view + 1) % 4;
                                                ui.reg_view = idx;
                                                ui.selected = 0;
                                                ui.scroll = 0;
                                                set_status(&mut ui, format!("View: {}", names[idx]));
                                            }
                                        }
                                        // Space 切换当前视图寄存器类型的读启用状态（客户端模式生效）
                                        KeyCode::Char(' ') => {
                                            if !is_monitor_mode {
                                                let mut s = state.write().await;
                                                s.read_enabled[ui.reg_view] = !s.read_enabled[ui.reg_view];
                                                let state_str = if s.read_enabled[ui.reg_view] { "Read enabled" } else { "Read disabled" };
                                                let idx = ui.reg_view;
                                                drop(s);
                                                let names = ["Holding", "Coils", "Discrete", "Input"];
                                                set_status(&mut ui, format!("{} {}", names[idx], state_str));
                                            }
                                        }
                                        // 从设备扫描：l（小写 L）
                                        KeyCode::Char('l') => {
                                            if !is_monitor_mode && !ui.edit_mode && !ui.show_monitor {
                                                let s = state.read().await;
                                                if s.slave_scan_running {
                                                    drop(s);
                                                    set_status(&mut ui, t!("run_ui.scan_running"));
                                                } else if let Some(ref _results) = s.slave_scan_result {
                                                    drop(s);
                                                    ui.show_scan_dialog = true;
                                                } else {
                                                    drop(s);
                                                    let mut s = state.write().await;
                                                    s.slave_scan_running = true;
                                                    s.slave_scan_result = None;
                                                    drop(s);
                                                    let tx_clone = tx.clone();
                                                    let state_clone = state.clone();
                                                    tokio::spawn(async move {
                                                        let (resp_tx, resp_rx) = oneshot::channel();
                                                        let _ = tx_clone.send(RegCmd::SlaveScan { resp: resp_tx });
                                                        let results = resp_rx.await.unwrap_or_default();
                                                        let mut s = state_clone.write().await;
                                                        s.slave_scan_result = Some(results);
                                                        s.slave_scan_running = false;
                                                    });
                                                    set_status(&mut ui, t!("run_ui.scan_started"));
                                                }
                                            }
                                        }
                                        // p: 打开当前选中寄存器的模式配置对话框（仅 holding 和 input 视图）
                                        KeyCode::Char('p') => {
                                            if !is_monitor_mode && !ui.edit_mode && !ui.show_monitor {
                                                let s = state.read().await;
                                                let reg_type = ui.reg_view;
                                                let addr = ui.selected;
                                                if reg_type == REG_VIEW_HOLDING && addr < s.holding.len() {
                                                    let pattern = if addr < s.holding_change_patterns.len() {
                                                        s.holding_change_patterns[addr]
                                                    } else {
                                                        crate::RegChangePattern::Random
                                                    };
                                                    let freq = if addr < s.holding_pattern_freqs.len() {
                                                        s.holding_pattern_freqs[addr]
                                                    } else {
                                                        1.0
                                                    };
                                                    let sel = pattern_index(&pattern);
                                                    drop(s);
                                                    ui.pattern_dialog_open = true;
                                                    ui.pattern_dialog_addr = addr;
                                                    ui.pattern_dialog_reg_type = REG_VIEW_HOLDING;
                                                    ui.pattern_dialog_sel = sel;
                                                    ui.pattern_dialog_freq = freq;
                                                    ui.pattern_dialog_editing_freq = false;
                                                    set_status(&mut ui, "Pattern config: ↑↓ select Enter confirm Esc cancel");
                                                } else if reg_type == REG_VIEW_INPUT && addr < s.input_registers.len() {
                                                    let pattern = if addr < s.input_change_patterns.len() {
                                                        s.input_change_patterns[addr]
                                                    } else {
                                                        crate::RegChangePattern::Random
                                                    };
                                                    let freq = if addr < s.input_pattern_freqs.len() {
                                                        s.input_pattern_freqs[addr]
                                                    } else {
                                                        1.0
                                                    };
                                                    let sel = pattern_index(&pattern);
                                                    drop(s);
                                                    ui.pattern_dialog_open = true;
                                                    ui.pattern_dialog_addr = addr;
                                                    ui.pattern_dialog_reg_type = REG_VIEW_INPUT;
                                                    ui.pattern_dialog_sel = sel;
                                                    ui.pattern_dialog_freq = freq;
                                                    ui.pattern_dialog_editing_freq = false;
                                                    set_status(&mut ui, "Pattern config: ↑↓ select Enter confirm Esc cancel");
                                                } else {
                                                    drop(s);
                                                    set_status(&mut ui, "Pattern config not available for this register type");
                                                }
                                            }
                                        }
                                        // V: 批量切换当前视图所有寄存器的值变化模拟
                                        KeyCode::Char('V') => {
                                            if !is_monitor_mode && !ui.edit_mode && !ui.show_monitor {
                                                let reg_type = ui.reg_view;
                                                if reg_type != REG_VIEW_HOLDING && reg_type != REG_VIEW_INPUT {
                                                    set_status(&mut ui, t!("run_ui.value_change_unsupported"));
                                                    continue;
                                                }
                                                let mut s = state.write().await;
                                                let reg_len = if reg_type == REG_VIEW_HOLDING {
                                                    s.holding.len()
                                                } else {
                                                    s.input_registers.len()
                                                };
                                                let enabled = if reg_type == REG_VIEW_HOLDING {
                                                    &mut s.holding_change_enabled
                                                } else {
                                                    &mut s.input_change_enabled
                                                };
                                                let current_on = enabled.iter().filter(|&&e| e).count();
                                                let total = reg_len;
                                                let new_state = current_on <= total / 2;
                                                enabled.resize(reg_len, false);
                                                for e in enabled.iter_mut() {
                                                    *e = new_state;
                                                }
                                                let reg_name = if reg_type == REG_VIEW_HOLDING { "Holding" } else { "Input" };
                                                drop(s);
                                                if new_state {
                                                    set_status(&mut ui, format!("{}: 全部开启变化 ({}/{})", reg_name, total, total));
                                                } else {
                                                    set_status(&mut ui, format!("{}: 全部关闭变化 (0/{})", reg_name, total));
                                                }
                                            }
                                        }
                                        // C: 清除值变化历史记录
                                        KeyCode::Char('C') => {
                                            if !is_monitor_mode && !ui.edit_mode {
                                                let mut s = state.write().await;
                                                let cleared = s.reg_change_history.len();
                                                s.reg_change_history.clear();
                                                s.reg_just_changed.clear();
                                                s.reg_change_direction.clear();
                                                s.reg_bar_history.clear();
                                                drop(s);
                                                set_status(&mut ui, format!("已清除 {} 条变化记录", cleared));
                                            }
                                        }
                                        // /: 搜索过滤寄存器
                                        KeyCode::Char('/') => {
                                            if !is_monitor_mode && !ui.edit_mode {
                                                ui.search_mode = true;
                                                ui.search_buf.clear();
                                                ui.status_msg = Some("/_ 搜索地址或标签".to_string());
                                            }
                                        }
                                        _ => {}
                                    }
                                }
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

pub(crate) fn is_secondary_register(
    addr: usize,
    combinations: &HashMap<usize, crate::RegDataFormat>,
) -> bool {
    for (&primary_addr, &fmt) in combinations {
        let count = fmt.regs_needed();
        if addr > primary_addr && addr < primary_addr + count {
            return true;
        }
    }
    false
}

/// 从给定地址开始向前查找下一个可见（非次级）的寄存器
pub(crate) fn next_visible_reg(
    from: usize,
    max: usize,
    combinations: &HashMap<usize, crate::RegDataFormat>,
) -> Option<usize> {
    (from + 1..max).find(|&i| !is_secondary_register(i, combinations))
}

/// 从给定地址开始向后查找上一个可见（非次级）的寄存器
pub(crate) fn prev_visible_reg(
    from: usize,
    combinations: &HashMap<usize, crate::RegDataFormat>,
) -> Option<usize> {
    (0..from)
        .rev()
        .find(|&i| !is_secondary_register(i, combinations))
}

/// 确保 selected 指向一个可见（非次级）寄存器。如果当前 selected 是次级寄存器，
/// 则向后寻找最近的一个可见寄存器；如果找不到，则向前寻找。
#[allow(dead_code)]
pub(crate) fn ensure_selected_visible(
    selected: &mut usize,
    max: usize,
    combinations: &HashMap<usize, crate::RegDataFormat>,
) {
    if is_secondary_register(*selected, combinations) {
        *selected = next_visible_reg(*selected, max, combinations)
            .or_else(|| prev_visible_reg(*selected, combinations))
            .unwrap_or(0);
    }
}

/// 渲染寄存器表格，支持所有 4 种寄存器类型
fn render_register_table(
    f: &mut ratatui::Frame<'_>,
    s: &AppState,
    ui: &mut Ui,
    area: ratatui::layout::Rect,
) {
    let visible_rows = area.height.saturating_sub(3) as usize;

    // 根据视图类型选择数据源和标签
    let (items, labels, is_bool) = match ui.reg_view {
        REG_VIEW_HOLDING => (
            &s.holding as &[u16],
            Some(&s.holding_label as &[String]),
            false,
        ),
        REG_VIEW_COILS => {
            // Convert Vec<bool> to Vec<u16> for uniform handling
            let mapped: Vec<u16> = s.coils.iter().map(|&b| if b { 1 } else { 0 }).collect();
            (
                Box::leak(mapped.into_boxed_slice()) as &[u16],
                None::<&[String]>,
                true,
            )
        }
        REG_VIEW_DISCRETE => {
            let mapped: Vec<u16> = s.discrete.iter().map(|&b| if b { 1 } else { 0 }).collect();
            (
                Box::leak(mapped.into_boxed_slice()) as &[u16],
                None::<&[String]>,
                true,
            )
        }
        REG_VIEW_INPUT => (&s.input_registers as &[u16], None::<&[String]>, false),
        _ => (
            &s.holding as &[u16],
            Some(&s.holding_label as &[String]),
            false,
        ),
    };

    let len = items.len();
    if ui.selected >= len {
        ui.selected = len.saturating_sub(1);
    }
    if ui.selected < ui.scroll {
        ui.scroll = ui.selected;
    }
    if visible_rows > 0 && ui.selected >= ui.scroll + visible_rows {
        ui.scroll = ui.selected + 1 - visible_rows;
    }

    // 标题
    let title = match ui.reg_view {
        REG_VIEW_HOLDING => t!("register_table.title"),
        REG_VIEW_COILS => t!("register_table.title_coils"),
        REG_VIEW_DISCRETE => t!("register_table.title_discrete"),
        REG_VIEW_INPUT => t!("register_table.title_input"),
        _ => t!("register_table.title"),
    };

    // 列头：线圈/离散输入不显示"备注"列
    let (col_labels, col_constraints): (Vec<Cell>, Vec<Constraint>) = if is_bool {
        (
            vec![
                Cell::from(t!("register_table.col_addr")),
                Cell::from(t!("register_table.col_value")),
            ],
            vec![Constraint::Length(18), Constraint::Min(16)],
        )
    } else if ui.show_change_bar {
        (
            vec![
                Cell::from(t!("register_table.col_addr")),
                Cell::from(t!("register_table.col_label")),
                Cell::from(t!("register_table.col_value")),
                Cell::from(t!("register_table.col_change")),
                Cell::from(t!("register_table.col_bar")),
            ],
            vec![
                Constraint::Length(18),
                Constraint::Length(36),
                Constraint::Min(10),
                Constraint::Length(6),
                Constraint::Length(BAR_HISTORY_SLOTS as u16),
            ],
        )
    } else {
        (
            vec![
                Cell::from(t!("register_table.col_addr")),
                Cell::from(t!("register_table.col_label")),
                Cell::from(t!("register_table.col_value")),
                Cell::from(t!("register_table.col_change")),
            ],
            vec![
                Constraint::Length(18),
                Constraint::Length(36),
                Constraint::Min(10),
                Constraint::Length(6),
            ],
        )
    };
    let header = Row::new(col_labels).style(
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    );

    // Determine combinations map for the current view
    let combinations = if is_bool {
        None
    } else {
        Some(match ui.reg_view {
            REG_VIEW_HOLDING => &s.holding_combinations,
            REG_VIEW_INPUT => &s.input_combinations,
            _ => &s.holding_combinations,
        })
    };

    // 搜索过滤：构建匹配索引列表
    let filtered_indices: Vec<usize> = if ui.search_buf.is_empty() {
        if is_bool {
            (0..len).collect()
        } else {
            (0..len)
                .filter(|&i| {
                    if let Some(combo) = combinations {
                        !is_secondary_register(i, combo)
                    } else {
                        true
                    }
                })
                .collect()
        }
    } else {
        let search_lower = ui.search_buf.to_lowercase();
        items
            .iter()
            .enumerate()
            .filter(|(i, _v)| {
                // 在搜索模式下，也排除次级寄存器
                if let Some(combo) = combinations {
                    if is_secondary_register(*i, combo) {
                        return false;
                    }
                }
                let idx_str = format!("{}", i);
                if idx_str.contains(&search_lower) {
                    return true;
                }
                if !is_bool {
                    if let Some(lbl) = labels.and_then(|l| l.get(*i)) {
                        if lbl.to_lowercase().contains(&search_lower) {
                            return true;
                        }
                    }
                }
                false
            })
            .map(|(i, _)| i)
            .collect()
    };
    let filtered_len = filtered_indices.len();

    // 调整 selected 到过滤列表中（也处理 selected 是次级寄存器的情况）
    if !filtered_indices.is_empty() && !filtered_indices.contains(&ui.selected) {
        ui.selected = filtered_indices[0];
        if ui.selected < ui.scroll {
            ui.scroll = ui.selected;
        }
    }

    // 由过滤列表驱动滚动
    if visible_rows > 0 && ui.scroll + visible_rows > filtered_len {
        ui.scroll = filtered_len.saturating_sub(visible_rows);
    }

    let rows = filtered_indices
        .iter()
        .skip(ui.scroll)
        .take(visible_rows.max(1))
        .map(|&i| {
            let v = &items[i];
            if is_bool {
                let val = if *v == 1 { "ON" } else { "OFF" };
                Row::new(vec![Cell::from(format!("{}", i)), Cell::from(val)])
            } else {
                // Determine the format for this register
                let reg_fmt = combinations
                    .and_then(|c| c.get(&i).copied())
                    .unwrap_or(ui.reg_format);
                let (swap_bytes, swap_words) = if ui.reg_view == REG_VIEW_INPUT {
                    (
                        s.input_swap_bytes.get(&i).copied().unwrap_or(false),
                        s.input_swap_words.get(&i).copied().unwrap_or(false),
                    )
                } else {
                    (
                        s.holding_swap_bytes.get(&i).copied().unwrap_or(false),
                        s.holding_swap_words.get(&i).copied().unwrap_or(false),
                    )
                };
                let mut val = format_register_value(items, i, reg_fmt, swap_bytes, swap_words);
                let mut label = labels.and_then(|l| l.get(i).cloned()).unwrap_or_default();
                // Show combination info in label if combined
                if let Some(&combo_fmt) = combinations.and_then(|c| c.get(&i)) {
                    let combo_label =
                        format!("[{}×{}]", combo_fmt.regs_needed(), combo_fmt.short_label());
                    if label.is_empty() {
                        label = combo_label;
                    } else {
                        label = format!("{} {}", combo_label, label);
                    }
                }
                // 编辑模式（仅 holding 支持编辑）
                if ui.reg_view == REG_VIEW_HOLDING
                    && ui.edit_mode
                    && i == ui.selected
                    && !ui.edit_is_profile
                {
                    if ui.edit_is_label {
                        label = ui.edit_buf.clone();
                    } else {
                        val = ui.edit_buf.clone();
                    }
                }
                let change_enabled = match ui.reg_view {
                    REG_VIEW_HOLDING => s.holding_change_enabled.get(i).copied().unwrap_or(false),
                    REG_VIEW_INPUT => s.input_change_enabled.get(i).copied().unwrap_or(false),
                    _ => false,
                };
                let change_str = if i < s.reg_just_changed.len() && s.reg_just_changed[i] {
                    format!("{}", s.reg_change_direction[i])
                } else if change_enabled {
                    t!("register_table.change_on").to_string()
                } else {
                    String::new()
                };
                if ui.show_change_bar {
                    let bar_spans = render_change_bar(s, i);
                    Row::new(vec![
                        Cell::from(format!("{}", i)),
                        Cell::from(label),
                        Cell::from(val),
                        Cell::from(change_str),
                        Cell::from(Line::from(bar_spans)),
                    ])
                } else {
                    Row::new(vec![
                        Cell::from(format!("{}", i)),
                        Cell::from(label),
                        Cell::from(val),
                        Cell::from(change_str),
                    ])
                }
            }
        });

    // 读启用状态指示
    let read_status = if s.read_enabled[ui.reg_view] {
        " [读]".to_string()
    } else {
        " [禁读]".to_string()
    };

    let mut table_state = TableState::default();
    // 在过滤列表中查找当前选中项的行索引
    let filtered_sel_pos = filtered_indices
        .iter()
        .position(|&x| x == ui.selected)
        .unwrap_or(0);
    table_state.select(Some(filtered_sel_pos.saturating_sub(ui.scroll)));

    let t = Table::new(rows, col_constraints)
        .header(header)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!("{}{}", title, read_status)),
        )
        .row_highlight_style(Style::default().bg(Color::Blue))
        .highlight_symbol(">> ");

    // 清理泄漏的内存（coils/discrete 的临时转换）
    if ui.reg_view == REG_VIEW_COILS || ui.reg_view == REG_VIEW_DISCRETE {
        // 对于 bool 类型，items 指向泄漏的内存，无法恢复；临时转换仅用于单帧渲染，泄漏很小
    }

    f.render_stateful_widget(t, area, &mut table_state);
}
fn render_change_bar(s: &AppState, addr: usize) -> Vec<Span<'static>> {
    let history = if addr < s.reg_bar_history.len() {
        &s.reg_bar_history[addr]
    } else {
        return vec![Span::styled(
            "·".repeat(BAR_HISTORY_SLOTS),
            Style::default().fg(Color::DarkGray),
        )];
    };
    if history.is_empty() {
        return vec![Span::styled(
            "·".repeat(BAR_HISTORY_SLOTS),
            Style::default().fg(Color::DarkGray),
        )];
    }
    let mut spans: Vec<Span<'static>> = Vec::with_capacity(BAR_HISTORY_SLOTS);
    // 第一个值无前驱，用暗色方块
    spans.push(Span::styled("·", Style::default().fg(Color::DarkGray)));
    for i in 1..history.len() {
        let prev = history[i - 1];
        let curr = history[i];
        if curr < prev {
            // 值变小 → 绿色
            spans.push(Span::styled("▃", Style::default().fg(Color::Green)));
        } else if curr > prev {
            // 值变大 → 红色
            spans.push(Span::styled("▇", Style::default().fg(Color::Red)));
        } else {
            // 无变化 → 暗色
            spans.push(Span::styled("·", Style::default().fg(Color::DarkGray)));
        }
    }
    // 填充剩余空位至 BAR_HISTORY_SLOTS
    while spans.len() < BAR_HISTORY_SLOTS {
        spans.push(Span::styled("·", Style::default().fg(Color::DarkGray)));
    }
    spans
}

fn render_pattern_dialog(f: &mut ratatui::Frame<'_>, ui: &Ui, s: &crate::AppState) {
    let patterns = ["Random", "Up/Down", "Sine", "Square", "Triangle"];
    let addr = ui.pattern_dialog_addr;

    let current_val = match ui.pattern_dialog_reg_type {
        REG_VIEW_HOLDING if addr < s.holding.len() => format_u16(s.holding[addr], ui.reg_format),
        REG_VIEW_INPUT if addr < s.input_registers.len() => {
            format_u16(s.input_registers[addr], ui.reg_format)
        }
        _ => "?".to_string(),
    };

    let reg_name = if ui.pattern_dialog_reg_type == REG_VIEW_HOLDING {
        "Holding"
    } else {
        "Input"
    };

    let mut text = format!(
        "Register {} [{}]\nCurrent: {}\n\n",
        addr, reg_name, current_val
    );

    for (i, name) in patterns.iter().enumerate() {
        let marker = if i == ui.pattern_dialog_sel {
            "●"
        } else {
            "○"
        };
        if i == ui.pattern_dialog_sel && ui.pattern_dialog_editing_freq {
            text.push_str(&format!("  {} {}   ←\n", marker, name));
        } else {
            text.push_str(&format!("  {} {}\n", marker, name));
        }
    }
    text.push('\n');

    let freq_str = if ui.pattern_dialog_editing_freq {
        format!("{} ", ui.pattern_dialog_freq_buf)
    } else {
        format!("{:.2} ", ui.pattern_dialog_freq)
    };
    text.push_str(&format!("Frequency: [{}] Hz\n", freq_str));
    text.push_str("\n↑↓ select  Enter confirm  Esc cancel");

    let dialog_area = centered_rect(50, 50, f.area());
    let dialog = ratatui::widgets::Paragraph::new(text)
        .block(
            ratatui::widgets::Block::default()
                .borders(ratatui::widgets::Borders::ALL)
                .title("Register Change Pattern")
                .border_style(Style::default().fg(Color::Yellow)),
        )
        .style(Style::default().fg(Color::White).bg(Color::Black));
    f.render_widget(ratatui::widgets::Clear, dialog_area);
    f.render_widget(dialog, dialog_area);
}

/// 渲染当前配置信息弹窗
fn render_profile_info(f: &mut ratatui::Frame<'_>, ui: &Ui) {
    let dialog_area = centered_rect(55, 65, f.area());
    f.render_widget(ratatui::widgets::Clear, dialog_area);

    let a = &ui.args;
    let mode = match a.main_mode.as_str() {
        "tcp-server" => "TCP Server",
        "tcp-client" => "TCP Client",
        "rtu-server" => "RTU Server",
        "rtu-client" => "RTU Client",
        "tcp-monitor" => "TCP Monitor",
        "rtu-monitor" => "RTU Monitor",
        _ => &a.main_mode,
    };
    let connection = if a.main_mode.contains("rtu") {
        format!(
            "{} | {:.1}K {}/{}/{}",
            a.device,
            a.baudrate as f64 / 1000.0,
            a.databits,
            a.parity.to_uppercase(),
            a.stopbits,
        )
    } else {
        format!("port {}", a.tcp_port)
    };
    let register_ranges = t!(
        "profile_info.register_ranges",
        h = a.holding_count,
        c = a.coil_count,
        i = a.input_count,
        d = a.discrete_count,
    )
    .to_string();
    let tick_info = if a.main_mode.contains("client") || a.main_mode.contains("monitor") {
        t!("profile_info.tick_ms", ms = a.client_tick_ms).to_string()
    } else {
        t!("profile_info.server_mode").to_string()
    };
    let data_fmt = ui.reg_format.short_label().to_string();
    let selected_profile = ui
        .monitor_selected_profile
        .as_deref()
        .or(ui.selected_profile.as_deref())
        .unwrap_or("-");
    let labels_count = a.labels.len();
    let combo_count = a.reg_combinations.len();

    let text = format!(
        "{}\n\n\
         {}\n{}\n\n\
         {}\n{}\n\n\
         {}\n{}\n\n\
         {}\n{}\n\n\
         {}\n{}\n\n\
         {}\n{}\n\n\
         {}\n{}",
        mode,
        t!("profile_info.connection"),
        connection,
        t!("profile_info.slave"),
        t!("profile_info.unit", unit = a.unit),
        t!("profile_info.registers"),
        register_ranges,
        t!("profile_info.timing"),
        tick_info,
        t!("profile_info.data_format"),
        data_fmt,
        t!("profile_info.labels_combos"),
        t!(
            "profile_info.labels_count",
            c1 = labels_count,
            c2 = combo_count
        ),
        t!("profile_info.profile"),
        selected_profile,
    );

    let dialog = ratatui::widgets::Paragraph::new(text)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(t!("profile_info.title"))
                .border_style(Style::default().fg(Color::Cyan)),
        )
        .style(Style::default().fg(Color::White).bg(Color::Black));
    f.render_widget(dialog, dialog_area);
}
