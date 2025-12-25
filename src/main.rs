use anyhow::{anyhow, Context, Result};
use clap::{Parser, ValueEnum};
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
    widgets::{Block, Borders, Cell, Row, Table, TableState},
    Terminal,
};
use std::{future, io, sync::Arc, time::Duration};
use tokio::sync::{mpsc, oneshot, RwLock};
use tokio_modbus::{prelude::*, server::rtu::Server};
use tokio_serial::{DataBits, FlowControl, Parity, SerialPortBuilderExt, StopBits};

#[derive(Copy, Clone, Debug, ValueEnum)]
enum DisplayBase {
    Dec,
    Hex,
    Bin,
}

#[derive(Parser, Debug)]
#[command(
    name = "modbus 工具箱",
    about = "简洁的 TUI 程序，包含 RTU/TCP 服务器/客户端 与静默侦听"
)]
#[derive(Clone)]
struct Args {
    /// Serial device path, e.g. /dev/ttyUSB0, COM3
    #[arg(short, long, default_value = "dev/null")]
    device: String,

    /// Slave/unit id (1..=247)
    #[arg(short = 'u', long, default_value_t = 1)]
    unit: u8,

    #[arg(short, long, default_value_t = 9600)]
    baudrate: u32,

    #[arg(long, default_value_t = 8)]
    databits: u8,

    #[arg(long, default_value = "n")]
    parity: String,

    #[arg(long, default_value_t = 1)]
    stopbits: u8,

    #[arg(long, default_value = "none")]
    flow: String,

    /// Number of holding registers to expose (starting at 0)
    #[arg(short = 'n', long, default_value_t = 512)]
    holding_count: usize,

    /// UI refresh period (ms)
    #[arg(long, default_value_t = 50)]
    ui_tick_ms: u64,

    /// Initial display base
    #[arg(long, value_enum, default_value_t = DisplayBase::Dec)]
    base: DisplayBase,
}

#[derive(Default)]
struct AppState {
    holding: Vec<u16>,
}

fn parse_parity(s: &str) -> Result<Parity> {
    match s.to_ascii_lowercase().as_str() {
        "n" | "none" => Ok(Parity::None),
        "e" | "even" => Ok(Parity::Even),
        "o" | "odd" => Ok(Parity::Odd),
        _ => Err(anyhow!("invalid parity: {s} (use n/e/o or none/even/odd)")),
    }
}
fn parse_flow(s: &str) -> Result<FlowControl> {
    match s.to_ascii_lowercase().as_str() {
        "none" => Ok(FlowControl::None),
        "hardware" | "hw" | "rtscts" => Ok(FlowControl::Hardware),
        "software" | "sw" | "xonxoff" => Ok(FlowControl::Software),
        _ => Err(anyhow!("invalid flow: {s} (use none/hardware/software)")),
    }
}
fn parse_databits(v: u8) -> Result<DataBits> {
    match v {
        5 => Ok(DataBits::Five),
        6 => Ok(DataBits::Six),
        7 => Ok(DataBits::Seven),
        8 => Ok(DataBits::Eight),
        _ => Err(anyhow!("invalid databits: {v} (use 5/6/7/8)")),
    }
}
fn parse_stopbits(v: u8) -> Result<StopBits> {
    match v {
        1 => Ok(StopBits::One),
        2 => Ok(StopBits::Two),
        _ => Err(anyhow!("invalid stopbits: {v} (use 1/2)")),
    }
}

fn format_u16(v: u16, base: DisplayBase) -> String {
    match base {
        DisplayBase::Dec => format!("{v}"),
        DisplayBase::Hex => format!("0x{v:04X}"),
        DisplayBase::Bin => format!("0b{v:016b}"),
    }
}

fn parse_u16_str(s: &str, base: DisplayBase) -> Result<u16> {
    let t = s.trim();
    if t.is_empty() {
        return Err(anyhow!("empty value"));
    }
    // Allow explicit prefixes regardless of current base.
    if let Some(rest) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        return u16::from_str_radix(rest, 16).context("parse hex");
    }
    if let Some(rest) = t.strip_prefix("0b").or_else(|| t.strip_prefix("0B")) {
        return u16::from_str_radix(rest, 2).context("parse bin");
    }
    match base {
        DisplayBase::Dec => t.parse::<u16>().context("parse dec"),
        DisplayBase::Hex => u16::from_str_radix(t, 16).context("parse hex"),
        DisplayBase::Bin => u16::from_str_radix(t, 2).context("parse bin"),
    }
}

// --- NEW: register worker command channel (avoids block_on in Service) ---
enum RegCmd {
    ReadHolding {
        addr: usize,
        cnt: usize,
        resp: oneshot::Sender<std::result::Result<Vec<u16>, ExceptionCode>>,
    },
    WriteSingle {
        addr: usize,
        value: u16,
        resp: oneshot::Sender<std::result::Result<(), ExceptionCode>>,
    },
    WriteMultiple {
        addr: usize,
        values: Vec<u16>,
        resp: oneshot::Sender<std::result::Result<(), ExceptionCode>>,
    },
}

async fn reg_worker_loop(
    state: Arc<RwLock<AppState>>,
    holding_len: usize,
    mut rx: mpsc::UnboundedReceiver<RegCmd>,
) {
    while let Some(cmd) = rx.recv().await {
        match cmd {
            RegCmd::ReadHolding { addr, cnt, resp } => {
                let out = if addr + cnt > holding_len {
                    Err(ExceptionCode::IllegalDataAddress)
                } else {
                    let s = state.read().await;
                    Ok(s.holding[addr..addr + cnt].to_vec())
                };
                let _ = resp.send(out);
            }
            RegCmd::WriteSingle { addr, value, resp } => {
                let out = if addr >= holding_len {
                    Err(ExceptionCode::IllegalDataAddress)
                } else {
                    let mut s = state.write().await;
                    s.holding[addr] = value;
                    Ok(())
                };
                let _ = resp.send(out);
            }
            RegCmd::WriteMultiple { addr, values, resp } => {
                let out = if addr + values.len() > holding_len {
                    Err(ExceptionCode::IllegalDataAddress)
                } else {
                    let mut s = state.write().await;
                    s.holding[addr..addr + values.len()].copy_from_slice(&values);
                    Ok(())
                };
                let _ = resp.send(out);
            }
        }
    }
}

// ---------------- Modbus server (holding registers only) ----------------

#[derive(Clone)]
struct HoldingService {
    tx: mpsc::UnboundedSender<RegCmd>,
    holding_len: usize,
    unit: u8,
}

impl tokio_modbus::server::Service for HoldingService {
    type Request = SlaveRequest<'static>;
    type Response = Response;
    type Exception = ExceptionCode;
    type Future = future::Ready<std::result::Result<Self::Response, Self::Exception>>;

    fn call(&self, req: Self::Request) -> Self::Future {
        // Enforce single unit id
        if req.slave != self.unit {
            return future::ready(Err(ExceptionCode::IllegalFunction));
        }

        match req.request {
            Request::ReadHoldingRegisters(addr, cnt) => {
                let addr = addr as usize;
                let cnt = cnt as usize;
                if addr + cnt > self.holding_len {
                    return future::ready(Err(ExceptionCode::IllegalDataAddress));
                }
                let (resp_tx, mut resp_rx) = oneshot::channel();
                let _ = self.tx.send(RegCmd::ReadHolding {
                    addr,
                    cnt,
                    resp: resp_tx,
                });
                // Must be ready immediately per trait; fall back to ServerDeviceFailure if worker stalled.
                match resp_rx.try_recv() {
                    Ok(Ok(values)) => future::ready(Ok(Response::ReadHoldingRegisters(values))),
                    Ok(Err(ex)) => future::ready(Err(ex)),
                    Err(_not_ready) => future::ready(Err(ExceptionCode::ServerDeviceFailure)),
                }
            }
            Request::WriteSingleRegister(addr, value) => {
                let addr = addr as usize;
                if addr >= self.holding_len {
                    return future::ready(Err(ExceptionCode::IllegalDataAddress));
                }
                let (resp_tx, mut resp_rx) = oneshot::channel();
                let _ = self.tx.send(RegCmd::WriteSingle {
                    addr,
                    value,
                    resp: resp_tx,
                });
                match resp_rx.try_recv() {
                    Ok(Ok(())) => {
                        future::ready(Ok(Response::WriteSingleRegister(addr as u16, value)))
                    }
                    Ok(Err(ex)) => future::ready(Err(ex)),
                    Err(_) => future::ready(Err(ExceptionCode::ServerDeviceFailure)),
                }
            }
            Request::WriteMultipleRegisters(addr, values) => {
                let addr_usize = addr as usize;
                if addr_usize + values.len() > self.holding_len {
                    return future::ready(Err(ExceptionCode::IllegalDataAddress));
                }
                let qty = values.len() as u16;
                let (resp_tx, mut resp_rx) = oneshot::channel();
                let _ = self.tx.send(RegCmd::WriteMultiple {
                    addr: addr_usize,
                    values: values.to_vec(),
                    resp: resp_tx,
                });
                match resp_rx.try_recv() {
                    Ok(Ok(())) => future::ready(Ok(Response::WriteMultipleRegisters(addr, qty))),
                    Ok(Err(ex)) => future::ready(Err(ex)),
                    Err(_) => future::ready(Err(ExceptionCode::ServerDeviceFailure)),
                }
            }
            _ => future::ready(Err(ExceptionCode::IllegalFunction)),
        }
    }
}

async fn run_modbus_rtu_server(
    args: Args,
    _state: Arc<RwLock<AppState>>,
    tx: mpsc::UnboundedSender<RegCmd>,
) -> Result<()> {
    let parity = parse_parity(&args.parity)?;
    let flow = parse_flow(&args.flow)?;
    let databits = parse_databits(args.databits)?;
    let stopbits = parse_stopbits(args.stopbits)?;

    let builder = tokio_serial::new(args.device.clone(), args.baudrate)
        .parity(parity)
        .flow_control(flow)
        .data_bits(databits)
        .stop_bits(stopbits);

    // async open (correct for tokio-modbus rtu server)
    let port = builder.open_native_async().context("open serial")?;

    let service = HoldingService {
        tx,
        holding_len: args.holding_count,
        unit: args.unit,
    };

    let server = Server::new(port);
    server
        .serve_forever(service)
        .await
        .map_err(|e| anyhow!("{e}"))?;
    Ok(())
}

// ---------------- Ratatui UI ----------------

struct Ui {
    base: DisplayBase,
    selected: usize,
    scroll: usize,
    edit_mode: bool,
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

async fn run_ui(
    state: Arc<RwLock<AppState>>,
    tx: mpsc::UnboundedSender<RegCmd>,
    args: Args,
    server_status: Arc<RwLock<Option<String>>>, // NEW
) -> Result<()> {
    enable_raw_mode().context("enable raw mode")?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen).context("enter alt screen")?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).context("create terminal")?;

    let mut events = EventStream::new();
    let mut ui = Ui::new(args.base);

    let tick = Duration::from_millis(args.ui_tick_ms);
    let mut interval = tokio::time::interval(tick);

    let res: Result<()> = loop {
        tokio::select! {
            _ = interval.tick() => {
                let s = state.read().await;
                let server_err = server_status.read().await.clone(); // NEW
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
                        Cell::from("值"),
                    ]).style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD));

                    let rows = s.holding
                        .iter()
                        .enumerate()
                        .skip(ui.scroll)
                        .take(visible_rows.max(1))
                        .map(|(i, v)| {
                            let mut val = format_u16(*v, ui.base);
                            if ui.edit_mode && i == ui.selected {
                                val = ui.edit_buf.clone();
                            }
                            Row::new(vec![
                                Cell::from(format!("{i}")),
                                Cell::from(val),
                            ])
                        });

                    let mut table_state = TableState::default();
                    table_state.select(Some(ui.selected.saturating_sub(ui.scroll)));

                    let t = Table::new(rows, [Constraint::Length(18), Constraint::Min(10)])
                        .header(header)
                        .block(Block::default().borders(Borders::ALL).title("保持型寄存器"))
                        .row_highlight_style(Style::default().bg(Color::Blue).fg(Color::White))
                        .highlight_symbol(">> ");


                    f.render_stateful_widget(t, chunks[0], &mut table_state);

                    // Status line priority: server/modbus error > local ui status > normal
                    let status_line = if let Some(m) = server_err.as_deref() {
                        format!("Modbus/RTU 错误: {m}")
                    } else if let Some(m) = ui.status_msg.as_deref() {
                        m.to_string()
                    } else if ui.edit_mode {
                        format!("修改 (格式={:?}) Enter=提交 Esc=取消 | 输入: {}", ui.base, ui.edit_buf)
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
                        "输入数据只接受数字; 可通过 0x/0b前缀指定格式; Backspace 退出输入"
                    } else {
                        "jk 移动 | e 编辑 | d/h/b 格式 | q 退出"
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
                    Event::Key(KeyEvent { code, modifiers, .. }) => {
                        // NEW: allow clearing server error from UI
                        if !ui.edit_mode && code == KeyCode::Char('c') && !modifiers.contains(KeyModifiers::CONTROL) {
                            *server_status.write().await = None;
                            ui.status_msg = None;
                        }

                        if !ui.edit_mode && (code == KeyCode::Char('q') || (code == KeyCode::Char('c') && modifiers.contains(KeyModifiers::CONTROL))) {
                            break Ok(());
                        }

                        if ui.edit_mode {
                            match code {
                                KeyCode::Esc => {
                                    ui.edit_mode = false;
                                    ui.edit_buf.clear();
                                    ui.status_msg = None;
                                }
                                KeyCode::Enter => {
                                    match parse_u16_str(&ui.edit_buf, ui.base) {
                                        Ok(new_val) => {
                                            let (resp_tx, resp_rx) = oneshot::channel();
                                            let _ = tx.send(RegCmd::WriteSingle { addr: ui.selected, value: new_val, resp: resp_tx });
                                            match resp_rx.await {
                                                Ok(Ok(())) => {
                                                    ui.edit_mode = false;
                                                    ui.edit_buf.clear();
                                                    ui.status_msg = None;
                                                }
                                                Ok(Err(ex)) => set_status(&mut ui, format!("Modbus exception: {ex:?}")),
                                                Err(_) => set_status(&mut ui, "Worker disconnected"),
                                            }
                                        }
                                        Err(e) => set_status(&mut ui, format!("Invalid value: {e}")),
                                    }
                                }
                                KeyCode::Backspace => {
                                    ui.edit_buf.pop();
                                    ui.status_msg = None;
                                }
                                KeyCode::Char(ch) => {
                                    if edit_accepts_char(&ui.edit_buf, ch, ui.base) {
                                        ui.edit_buf.push(ch);
                                        ui.status_msg = None;
                                    } else {
                                        set_status(&mut ui, "Rejected character for current base");
                                    }
                                }
                                _ => {}
                            }
                        } else {
                            match code {
                                 KeyCode::PageDown => {
                                    let len = state.read().await.holding.len();
                                    ui.selected = len;
                                }
                                KeyCode::PageUp => {
                                    ui.selected = 0;
                                }

                                KeyCode::Char('k') => {
                                    ui.selected = ui.selected.saturating_sub(1);
                                }
                                KeyCode::Char('j') => {
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
                                KeyCode::Char('e') => {
                                    let s = state.read().await;
                                    if ui.selected < s.holding.len() {
                                        ui.edit_mode = true;
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
        };
    };

    disable_raw_mode().ok();
    execute!(terminal.backend_mut(), LeaveAlternateScreen).ok();
    terminal.show_cursor().ok();

    res
}

// ---------------- main ----------------

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    let holding = vec![0u16; args.holding_count];
    let state = Arc::new(RwLock::new(AppState { holding }));

    // NEW: shared server error/status
    let server_status: Arc<RwLock<Option<String>>> = Arc::new(RwLock::new(None));

    // worker channel
    let (tx, rx) = mpsc::unbounded_channel::<RegCmd>();
    tokio::spawn(reg_worker_loop(Arc::clone(&state), args.holding_count, rx));

    // server (NEW: push errors into server_status)
    let server_args = args.clone();
    let server_state = Arc::clone(&state);
    let server_tx = tx.clone();
    let server_status_bg = Arc::clone(&server_status);
    let server_task = tokio::spawn(async move {
        if let Err(e) = run_modbus_rtu_server(server_args, server_state, server_tx).await {
            *server_status_bg.write().await = Some(format!("{e:#}"));
            return Err(e);
        }
        Ok::<_, anyhow::Error>(())
    });

    // ui (pass server_status)
    let ui_res = run_ui(Arc::clone(&state), tx, args, server_status).await;

    server_task.abort();
    let _ = server_task.await;

    ui_res
}
