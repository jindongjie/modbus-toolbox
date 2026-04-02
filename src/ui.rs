use anyhow::{anyhow, Context, Result};
use futures::StreamExt;
use tokio::sync::{mpsc, oneshot, RwLock};

// std
use std::{collections::HashMap, io, sync::Arc, time::Duration};

// tokio

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
    widgets::{Block, Borders, Cell, Row, Table, TableState},
    Terminal,
};

// crate
use crate::{format_u16, modbus::RegCmd, parse_u16_str, AppState, Args, DisplayBase};
pub struct Ui {
    base: DisplayBase,
    selected: usize,
    scroll: usize,
    edit_mode: bool,
    edit_is_label: bool,
    edit_is_profile: bool,
    edit_buf: String,
    status_msg: Option<String>,
}

impl Ui {
    fn new(base: DisplayBase) -> Self {
        Self {
            base,
            selected: 0,
            scroll: 0,
            edit_mode: false,
            edit_is_label: false,
            edit_is_profile: false,
            edit_buf: String::new(),
            status_msg: None,
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

pub async fn run_ui(
    state: Arc<RwLock<AppState>>,
    tx: mpsc::UnboundedSender<RegCmd>,
    args: Args,
    server_status: Arc<RwLock<Option<String>>>,
) -> Result<()> {
    enable_raw_mode().context("enable raw mode 进入终端全自主操作模式")?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen).context("enter alt screen 转入自主屏幕空间")?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).context("create terminal 创建终端")?;

    let mut events = EventStream::new();
    let mut ui = Ui::new(args.base);

    let tick = Duration::from_millis(args.ui_tick_ms);
    let mut interval = tokio::time::interval(tick);

    let res: Result<()> = loop {
        tokio::select! {
                    _ = interval.tick() => {
                        let s = state.read().await;
                        let server_err = server_status.read().await.clone();
                        terminal.draw(|f| {
                            let chunks = Layout::vertical([
                                Constraint::Min(5),
                                Constraint::Length(3),
                                Constraint::Length(3),
                            ]).split(f.area());

                            let visible_rows = chunks[0].height.saturating_sub(3) as usize;
                            if ui.selected >= s.holding.len() {
                                ui.selected = s.holding.len().saturating_sub(1);
                            }
                            if ui.selected < ui.scroll { ui.scroll = ui.selected; }
                            if visible_rows > 0 && ui.selected >= ui.scroll + visible_rows {
                                ui.scroll = ui.selected + 1 - visible_rows;
                            }

                            let header = Row::new(vec![
                                Cell::from("寄存器地址"),
                                Cell::from("备注标记"),
                                Cell::from("值"),
                            ]).style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD));

                            let rows = s.holding
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
                                    Row::new(vec![
                                        Cell::from(format!("{i}")),
                                        Cell::from(label),
                                        Cell::from(val),
                                    ])
                                });

                            let mut table_state = TableState::default();
                            table_state.select(Some(ui.selected.saturating_sub(ui.scroll)));

                            let t = Table::new(rows, [Constraint::Length(18), Constraint::Length(42), Constraint::Min(10)])
                                .header(header)
                                .block(Block::default().borders(Borders::ALL).title("保持型寄存器"))
                                .row_highlight_style(Style::default().bg(Color::Blue).fg(Color::White))
                                .highlight_symbol(">> ");

                            f.render_stateful_widget(t, chunks[0], &mut table_state);

                            let status_line = if let Some(m) = server_err.as_deref() {
                                format!("Modbus 错误: {m}")
                            } else if let Some(m) = ui.status_msg.as_deref() {
                                m.to_string()
                            } else if ui.edit_mode {
                                if ui.edit_is_profile {
                                    format!("保存当前配置 Enter=提交 Esc=取消 | 预设名: {}", ui.edit_buf)
                                } else if ui.edit_is_label {
                                    format!("修改备注 Enter=提交 Esc=取消 | 输入: {}", ui.edit_buf)
                                } else {
                                    format!("修改格式 (格式={:?}) Enter=提交 Esc=取消 | 输入: {}", ui.base, ui.edit_buf)
                                }
                            } else {
                                format!("格式={:?}", ui.base)
                            };

                            f.render_widget(
                                ratatui::widgets::Paragraph::new(status_line)
                                    .block(Block::default().borders(Borders::ALL).title("状态"))
                                    .style(if server_err.is_some() {
                                        Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
                                    } else {
                                        Style::default()
                                    }),
                                chunks[1],
                            );

                            let help = if ui.edit_mode {
                                format!("输入数据 {:?}; Backspace 退出 | m 100个寄存器编辑(仅数值)",ui.edit_buf)
                            } else {
                                "jk ↑↓ 移动 | e 编辑数值 | t 编辑备注 | o 保存配置 | d/h/b 格式 | q 退出".to_string()
                            };

                            f.render_widget(
                                ratatui::widgets::Paragraph::new(help)
                                    .block(Block::default().borders(Borders::ALL).title("帮助")),
                                chunks[2],
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
                        Ok(_) => set_status(&mut ui, format!("成功保存预设 [{}] 到 {}", profile_name, args.config)),
                        Err(e) => set_status(&mut ui, format!("写入文件失败: {}", e)),
                    },
                    Err(e) => set_status(&mut ui, format!("配置序列化失败: {}", e)),
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
                        set_status(&mut ui, format!("Invalid value 非法输入值: {e}"));
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

                                                        match resp_rx.await {
                                                            Ok(Ok(())) => {
                                                                ui.edit_mode = false;
                                                                ui.edit_buf.clear();
                                                                ui.status_msg = None;
                                                            }
                                                            Ok(Err(ex)) => set_status(
                                                                &mut ui,
                                                                format!("Modbus exception 异常: {ex:?}"),
                                                            ),
                                                            Err(_) => set_status(
                                                                &mut ui,
                                                                "Worker disconnected 运行中断",
                                                            ),
                                                        }
                                                    }
                                                    Err(e) => set_status(
                                                        &mut ui,
                                                        format!("Invalid value: {e} 非法输入值"),
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
                                                    "Rejected character for current base 无法输入该字符",
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
                                            ui.selected = ui.selected.saturating_sub(1);
                                        }
                                        KeyCode::Char('j') | KeyCode::Down => {
                                            let len = state.read().await.holding.len();
                                            ui.selected = (ui.selected + 1).min(len.saturating_sub(1));
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
