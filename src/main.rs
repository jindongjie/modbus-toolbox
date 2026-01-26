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
use std::{io, sync::Arc, time::Duration};
use tokio::sync::{mpsc, oneshot, RwLock};
use tokio_modbus::{prelude::*, server};
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
    about = "TUI 程序，包含 RTU/TCP 服务器/客户端与静默侦听"
)]
#[derive(Clone)]
struct Args {
    /// 主模式 1.tcp-服务端: tcp-server/ts 2.tcp-客户端 tcp-client/tc 3.rtu-服务端 rtu-server/rs 4.rtu-客户端 rtu-client/rs
    //  main mode 1.tcp-server/ts 2.tcp-client/tc 3.rtu-server/rs 4.rtu-client/rc
    #[arg(short = 'm', long, default_value = "tcp-client")]
    main_mode: String,

    /// TCP 端口号
    ///TCP port number(0~65535)
    #[arg(short = 'p', long, default_value_t = 502)]
    tcp_port: u16,

    /// 从设备地址/标识符（1~247)
    /// Slave/unit id (1~247)
    #[arg(short = 'u', long, default_value_t = 1)]
    unit: u8,

    /// 保持型寄存器列表长度 客户端为轮询的范围 服务端为暴露的范围（0~value)
    /// Number of holding registers to expose (starting at 0)
    #[arg(short = 'c', long, default_value_t = 512)]
    holding_count: usize,

    /// 客户端模式 轮询间隔(ms)
    /// client mode query interval period (ms)
    #[arg(long, default_value_t = 200)]
    client_tick_ms: u64,

    /// UI 刷新间隔(ms)
    /// UI refresh period (ms)
    #[arg(long, default_value_t = 10)]
    ui_tick_ms: u64,

    /// 串口设备路径, 例： /dev/ttyUSB0
    /// Serial Port device path eg: /dev/ttyUSB0
    #[arg(short, long, default_value = "dev/null")]
    device: String,

    ///串口波特率 合适值，根据串口驱动允许的最大波特率为上限
    ///Serial Port baudrate, fllow the serial port driver maxium as it up limit.
    #[arg(short, long, default_value_t = 9600)]
    baudrate: u32,

    ///串口数据位 5/6/7/8
    #[arg(long, default_value_t = 8)]
    databits: u8,

    ///串口校验位 n=none e=even o=odd
    #[arg(long, default_value = "n")]
    parity: String,

    ///串口停止位 1/2
    #[arg(long, default_value_t = 1)]
    stopbits: u8,

    ///串口流控 none=无 soft=软件 hard=硬件
    #[arg(long, default_value = "none")]
    flow: String,

    /// 界面初始化时间
    /// Initial display base
    #[arg(long, value_enum, default_value_t = DisplayBase::Dec)]
    base: DisplayBase,
}

#[derive(Default)]
struct AppState {
    holding: Vec<u16>,
}

enum MainMode {
    TcpServer,
    TcpClient,
    RTUServer,
    RTUClient,
}

fn parse_mainmode(s: &str) -> Result<MainMode> {
    match s.to_ascii_lowercase().as_str() {
        "ts" | "tcp-server" => Ok(MainMode::TcpServer),
        "tc" | "tcp-client" => Ok(MainMode::TcpClient),
        "rs" | "rtu-server" => Ok(MainMode::RTUServer),
        "rc" | "rtu-client" => Ok(MainMode::RTUClient),
        _ => Err(anyhow!(
            "invalid main mode 非法的主模式 : {s} (use ts/tc/rs/rc or refrence to help)"
        )),
    }
}

fn parse_parity(s: &str) -> Result<Parity> {
    match s.to_ascii_lowercase().as_str() {
        "n" | "none" => Ok(Parity::None),
        "e" | "even" => Ok(Parity::Even),
        "o" | "odd" => Ok(Parity::Odd),
        _ => Err(anyhow!(
            "invalid parity 非法串口校验位: {s} (use n/e/o or none/even/odd)"
        )),
    }
}
fn parse_flow(s: &str) -> Result<FlowControl> {
    match s.to_ascii_lowercase().as_str() {
        "none" => Ok(FlowControl::None),
        "hard" | "hw" | "rtscts" => Ok(FlowControl::Hardware),
        "soft" | "sw" | "xonxoff" => Ok(FlowControl::Software),
        _ => Err(anyhow!(
            "invalid flow 非法串口流控: {s} (use none/hardware/software)"
        )),
    }
}
fn parse_databits(v: u8) -> Result<DataBits> {
    match v {
        5 => Ok(DataBits::Five),
        6 => Ok(DataBits::Six),
        7 => Ok(DataBits::Seven),
        8 => Ok(DataBits::Eight),
        _ => Err(anyhow!(
            "invalid databits 非法串口数据位: {v} (use 5/6/7/8)"
        )),
    }
}
fn parse_stopbits(v: u8) -> Result<StopBits> {
    match v {
        1 => Ok(StopBits::One),
        2 => Ok(StopBits::Two),
        _ => Err(anyhow!("invalid stopbits 非法串口停止位: {v} (use 1/2)")),
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
        return Err(anyhow!("empty value 空值"));
    }
    //允许通过前缀强制定义输入类型
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

//寄存器指令枚举
enum RegCmd {
    ReadHolding {
        addr: usize,
        cnt: usize,
        resp: oneshot::Sender<std::result::Result<Vec<u16>, ExceptionCode>>,
    },
    WriteSingleHolding {
        addr: usize,
        value: u16,
        resp: oneshot::Sender<std::result::Result<(), ExceptionCode>>,
    },
    WriteMultipleHolding {
        addr: usize,
        values: Vec<u16>,
        resp: oneshot::Sender<std::result::Result<(), ExceptionCode>>,
    },
}

//寄存器指令执行循环
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
            RegCmd::WriteSingleHolding { addr, value, resp } => {
                let out = if addr >= holding_len {
                    Err(ExceptionCode::IllegalDataAddress)
                } else {
                    let mut s = state.write().await;
                    s.holding[addr] = value;
                    Ok(())
                };
                let _ = resp.send(out);
            }
            RegCmd::WriteMultipleHolding { addr, values, resp } => {
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

//保持型寄存器服务
#[derive(Clone)]
struct HoldingService {
    tx: mpsc::UnboundedSender<RegCmd>,
    holding_len: usize,
    unit: u8,
}

//Modbus 保持型寄存器服务
impl tokio_modbus::server::Service for HoldingService {
    type Request = SlaveRequest<'static>;
    type Response = Response;
    type Exception = ExceptionCode;
    type Future = std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Self::Response, Self::Exception>> + Send>,
    >;

    fn call(&self, req: Self::Request) -> Self::Future {
        //检查 slaveID 是否正确
        if req.slave != self.unit {
            return Box::pin(async { Err(ExceptionCode::IllegalFunction) });
        }

        match req.request {
            Request::ReadHoldingRegisters(addr, cnt) => {
                let addr = addr as usize;
                let cnt = cnt as usize;
                let holding_len = self.holding_len;
                let tx = self.tx.clone();

                Box::pin(async move {
                    if addr + cnt > holding_len {
                        return Err(ExceptionCode::IllegalDataAddress);
                    }

                    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
                    if tx
                        .send(RegCmd::ReadHolding {
                            addr,
                            cnt,
                            resp: resp_tx,
                        })
                        .is_err()
                    {
                        return Err(ExceptionCode::ServerDeviceFailure);
                    }

                    match resp_rx.await {
                        Ok(Ok(values)) => Ok(Response::ReadHoldingRegisters(values)),
                        Ok(Err(ex)) => Err(ex),
                        Err(_closed) => Err(ExceptionCode::ServerDeviceFailure),
                    }
                })
            }

            Request::WriteSingleRegister(addr, value) => {
                let addr = addr as usize;
                let holding_len = self.holding_len;
                let tx = self.tx.clone();

                Box::pin(async move {
                    if addr >= holding_len {
                        return Err(ExceptionCode::IllegalDataAddress);
                    }

                    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
                    if tx
                        .send(RegCmd::WriteSingleHolding {
                            addr,
                            value,
                            resp: resp_tx,
                        })
                        .is_err()
                    {
                        return Err(ExceptionCode::ServerDeviceFailure);
                    }

                    match resp_rx.await {
                        Ok(Ok(())) => Ok(Response::WriteSingleRegister(addr as u16, value)),
                        Ok(Err(ex)) => Err(ex),
                        Err(_closed) => Err(ExceptionCode::ServerDeviceFailure),
                    }
                })
            }

            Request::WriteMultipleRegisters(addr, values) => {
                let addr_usize = addr as usize;
                let holding_len = self.holding_len;
                let tx = self.tx.clone();
                let values_vec = values.to_vec();
                let qty = values_vec.len() as u16;

                Box::pin(async move {
                    if addr_usize + values_vec.len() > holding_len {
                        return Err(ExceptionCode::IllegalDataAddress);
                    }

                    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
                    if tx
                        .send(RegCmd::WriteMultipleHolding {
                            addr: addr_usize,
                            values: values_vec,
                            resp: resp_tx,
                        })
                        .is_err()
                    {
                        return Err(ExceptionCode::ServerDeviceFailure);
                    }

                    match resp_rx.await {
                        Ok(Ok(())) => Ok(Response::WriteMultipleRegisters(addr, qty)),
                        Ok(Err(ex)) => Err(ex),
                        Err(_closed) => Err(ExceptionCode::ServerDeviceFailure),
                    }
                })
            }

            _ => Box::pin(async { Err(ExceptionCode::IllegalFunction) }),
        }
    }
}

//各模式分支函数
async fn run_modbus_tcp_server(
    args: Args,
    _state: Arc<RwLock<AppState>>,
    tx: mpsc::UnboundedSender<RegCmd>,
) -> Result<()> {
    //创建新连接，不需要挂锁
    let port = tokio::time::timeout(
        Duration::from_millis(self.timeout_ms),
        TcpStream::connect(format!("{}:{}", unit.ip, unit.port)),
    )
    .await
    .context("连接Modbus设备超时")?
    .context("连接Modbus设备失败")?;

    port.set_nodelay(true).context("无法设置 TCP_NODELAY")?;

    let service = HoldingService {
        tx,
        holding_len: args.holding_count,
        unit: args.unit,
    };

    let server = server::tcp::Server::new(port);
    server
        .serve_forever(service)
        .await
        .map_err(|e| anyhow!("{e}"))?;
    Ok(())
}
async fn run_modbus_tcp_client(
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
    let port = builder
        .open_native_async()
        .context("open serial 正在打开串口")?;

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

async fn run_modbus_rtu_client(
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
    let port = builder
        .open_native_async()
        .context("open serial 正在打开串口")?;

    let service = HoldingService {
        tx,
        holding_len: args.holding_count,
        unit: args.unit,
    };

    let server = server::rtu::Server::new(port);
    server
        .serve_forever(service)
        .await
        .map_err(|e| anyhow!("{e}"))?;
    Ok(())
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

    let server = server::rtu::Server::new(port);
    server
        .serve_forever(service)
        .await
        .map_err(|e| anyhow!("{e}"))?;
    Ok(())
}

// ---------------- RataTUI ----------------

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
                        "输入数据只接受数字; 可通过 0x/0b前缀指定格式; Backspace 退出输入 | m 100个寄存器编辑"
                    } else {
                        "jk ↑↓ 移动 | e 编辑 | d/h/b 格式 | q 退出"
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
                    Event::Key(KeyEvent { code, modifiers,kind, .. }) => {
                        if kind != crossterm::event::KeyEventKind::Press {
                            continue;
                        }
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
                                            let _ = tx.send(RegCmd::WriteSingleHolding { addr: ui.selected, value: new_val, resp: resp_tx });
                                            match resp_rx.await {
                                                Ok(Ok(())) => {
                                                    ui.edit_mode = false;
                                                    ui.edit_buf.clear();
                                                    ui.status_msg = None;
                                                }
                                                Ok(Err(ex)) => set_status(&mut ui, format!("Modbus exception 异常: {ex:?}")),
                                                Err(_) => set_status(&mut ui, "Worker disconnected 运行中断"),
                                            }
                                        }
                                        Err(e) => set_status(&mut ui, format!("Invalid value 非法输入值: {e}")),
                                    }
                                }
                                KeyCode::Backspace => {
                                    ui.edit_buf.pop();
                                    ui.status_msg = None;
                                }
                                KeyCode::Char('m') => {
                                    match parse_u16_str(&ui.edit_buf, ui.base) {
                                        Ok(new_val) => {
                                            let (resp_tx, resp_rx) = oneshot::channel();
                                            let values = vec![new_val;100];
                                            let _ = tx.send(RegCmd::WriteMultipleHolding { addr: (ui.selected), values: (values), resp: (resp_tx) });
                                            match resp_rx.await {
                                                Ok(Ok(())) => {
                                                    ui.edit_mode = false;
                                                    ui.edit_buf.clear();
                                                    ui.status_msg = None;
                                                }
                                                Ok(Err(ex)) => set_status(&mut ui, format!("Modbus exception 异常: {ex:?}")),
                                                Err(_) => set_status(&mut ui, "Worker disconnected 运行中断"),
                                            }
                                        }
                                        Err(e) => set_status(&mut ui, format!("Invalid value: {e} 非法输入值")),
                                    }
                                }

                                KeyCode::Char(ch) => {
                                    if edit_accepts_char(&ui.edit_buf, ch, ui.base) {
                                        ui.edit_buf.push(ch);
                                        ui.status_msg = None;
                                    } else {
                                        set_status(&mut ui, "Rejected character for current base 无法输入该字符");
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

                                KeyCode::Char('k') | KeyCode::Up => {
                                    ui.selected = ui.selected.saturating_sub(1);
                                }
                                KeyCode::Char('j') | KeyCode::Down  => {
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

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    let main_mode = parse_mainmode(&args.main_mode)?;
    let holding = vec![0u16; args.holding_count];
    let state = Arc::new(RwLock::new(AppState { holding }));

    let server_status: Arc<RwLock<Option<String>>> = Arc::new(RwLock::new(None));

    // worker channel
    let (tx, rx) = mpsc::unbounded_channel::<RegCmd>();
    tokio::spawn(reg_worker_loop(Arc::clone(&state), args.holding_count, rx));

    let inner_args = args.clone();
    let inner_state = Arc::clone(&state);
    let inner_tx = tx.clone();
    let inner_status_bg = Arc::clone(&server_status);
    let inner_task = tokio::spawn(async move {
        match main_mode {
            MainMode::RTUServer => {
                if let Err(e) = run_modbus_rtu_server(inner_args, inner_state, inner_tx).await {
                    *inner_status_bg.write().await = Some(format!("{e:#}"));
                    return Err(e);
                }
            }
            MainMode::RTUClient => {
                if let Err(e) = run_modbus_rtu_client(inner_args, inner_state, inner_tx).await {
                    *inner_status_bg.write().await = Some(format!("{e:#}"));
                    return Err(e);
                }
            }

            MainMode::TcpServer => {
                if let Err(e) = run_modbus_tcp_server(inner_args, inner_state, inner_tx).await {
                    *inner_status_bg.write().await = Some(format!("{e:#}"));
                    return Err(e);
                }
            }
            MainMode::TcpClient => {
                if let Err(e) = run_modbus_tcp_client(inner_args, inner_state, inner_tx).await {
                    *inner_status_bg.write().await = Some(format!("{e:#}"));
                    return Err(e);
                }
            }
        }
        Ok::<_, anyhow::Error>(())
    });

    let ui_res = run_ui(Arc::clone(&state), tx, args, server_status).await;

    inner_task.abort();
    let _ = inner_task.await;

    ui_res
}
