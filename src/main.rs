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
use std::{collections::HashMap, io, sync::Arc, time::Duration};
use tokio::{
    net::TcpListener,
    sync::{mpsc, oneshot, RwLock},
};
use tokio_modbus::{
    prelude::*,
    server::{self, tcp::accept_tcp_connection},
};
use tokio_serial::{DataBits, FlowControl, Parity, SerialPortBuilderExt, StopBits};

use serde;

#[derive(Copy, Clone, Debug, ValueEnum, Default, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
enum DisplayBase {
    #[default]
    Dec,
    Hex,
    Bin,
}

#[derive(Parser, Debug, Clone, serde::Deserialize, serde::Serialize)]
#[command(
    name = "modbus 工具箱",
    about = "TUI 程序，包含 RTU/TCP 服务器/客户端与静默侦听"
)]
#[serde(default)]
struct Args {
    /// 配置文件名 (默认: config.toml)
    #[arg(long, default_value = "config.toml")]
    #[serde(skip)]
    config: String,

    /// 选择配置文件中的预设槽位 (若提供则优先使用配置文件的设置)
    #[arg(long)]
    #[serde(skip)]
    profile: Option<String>,

    /// 主模式 1.tcp-服务端: tcp-server/ts 2.tcp-客户端 tcp-client/tc 3.rtu-服务端 rtu-server/rs 4.rtu-客户端 rtu-client/rs
    #[arg(short = 'm', long, default_value = "tcp-client")]
    main_mode: String,

    /// TCP 端口号(1~65535)
    #[arg(short = 'p', long, default_value_t = 502)]
    tcp_port: u16,

    /// 从设备地址/标识符（1~247)
    #[arg(short = 'u', long, default_value_t = 1)]
    unit: u8,

    /// 保持型寄存器列表长度 客户端为轮询的范围 服务端为暴露的范围（0~value)
    #[arg(short = 'c', long, default_value_t = 1024)]
    holding_count: usize,

    /// 客户端模式 轮询间隔(ms)
    #[arg(long, default_value_t = 200)]
    client_tick_ms: u64,

    /// UI 刷新间隔(ms)
    #[arg(long, default_value_t = 10)]
    ui_tick_ms: u64,

    /// 串口设备路径, 例： /dev/ttyUSB0
    #[arg(short, long, default_value = "dev/null")]
    device: String,

    ///串口波特率 合适值，根据串口驱动允许的最大波特率为上限
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
    #[arg(long, value_enum, default_value_t = DisplayBase::Dec)]
    base: DisplayBase,

    /// 寄存器备注标签
    #[arg(skip)]
    #[serde(default)]
    labels: HashMap<String, String>,
}

impl Default for Args {
    fn default() -> Self {
        Self {
            config: "config.toml".into(),
            profile: None,
            main_mode: "tcp-client".into(),
            tcp_port: 502,
            unit: 1,
            holding_count: 512,
            client_tick_ms: 200,
            ui_tick_ms: 10,
            device: "dev/null".into(),
            baudrate: 9600,
            databits: 8,
            parity: "n".into(),
            stopbits: 1,
            flow: "none".into(),
            base: DisplayBase::Dec,
            labels: HashMap::new(),
        }
    }
}

#[derive(Default)]
struct AppState {
    holding: Vec<u16>,
    holding_label: Vec<String>,
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
            "非法的主模式 : {s} (use ts/tc/rs/rc or refrence to help)"
        )),
    }
}

fn parse_parity(s: &str) -> Result<Parity> {
    match s.to_ascii_lowercase().as_str() {
        "n" | "none" => Ok(Parity::None),
        "e" | "even" => Ok(Parity::Even),
        "o" | "odd" => Ok(Parity::Odd),
        _ => Err(anyhow!("非法串口校验位: {s} (use n/e/o or none/even/odd)")),
    }
}
fn parse_flow(s: &str) -> Result<FlowControl> {
    match s.to_ascii_lowercase().as_str() {
        "none" => Ok(FlowControl::None),
        "hard" | "hw" | "rtscts" => Ok(FlowControl::Hardware),
        "soft" | "sw" | "xonxoff" => Ok(FlowControl::Software),
        _ => Err(anyhow!("非法串口流控: {s} (use none/hardware/software)")),
    }
}
fn parse_databits(v: u8) -> Result<DataBits> {
    match v {
        5 => Ok(DataBits::Five),
        6 => Ok(DataBits::Six),
        7 => Ok(DataBits::Seven),
        8 => Ok(DataBits::Eight),
        _ => Err(anyhow!("非法串口数据位: {v} (use 5/6/7/8)")),
    }
}
fn parse_stopbits(v: u8) -> Result<StopBits> {
    match v {
        1 => Ok(StopBits::One),
        2 => Ok(StopBits::Two),
        _ => Err(anyhow!("非法串口停止位: {v} (use 1/2)")),
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
        return Err(anyhow!("空字符"));
    }
    //允许通过前缀强制定义输入类型
    if let Some(rest) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        return u16::from_str_radix(rest, 16).context("解析十六进制字符前缀");
    }
    if let Some(rest) = t.strip_prefix("0b").or_else(|| t.strip_prefix("0B")) {
        return u16::from_str_radix(rest, 2).context("解析二进制字符前缀");
    }
    match base {
        DisplayBase::Dec => t.parse::<u16>().context("解析十进制字符"),
        DisplayBase::Hex => u16::from_str_radix(t, 16).context("解析十六进制字符"),
        DisplayBase::Bin => u16::from_str_radix(t, 2).context("解析二进制字符"),
    }
}
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
        Box<
            dyn std::future::Future<Output = std::result::Result<Self::Response, Self::Exception>>
                + Send,
        >,
    >;

    fn call(&self, req: Self::Request) -> Self::Future {
        fn boxed<F>(fut: F) -> <HoldingService as tokio_modbus::server::Service>::Future
        where
            F: std::future::Future<Output = std::result::Result<Response, ExceptionCode>>
                + Send
                + 'static,
        {
            Box::pin(fut)
        }

        //检查 slaveID 是否正确
        if req.slave != self.unit {
            return boxed(async { Err(ExceptionCode::IllegalFunction) });
        }

        match req.request {
            Request::ReadHoldingRegisters(addr, cnt) => {
                let addr = addr as usize;
                let cnt = cnt as usize;
                let holding_len = self.holding_len;
                let tx: mpsc::UnboundedSender<RegCmd> = self.tx.clone();

                boxed(async move {
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

                boxed(async move {
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

                boxed(async move {
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

            _ => boxed(async { Err(ExceptionCode::IllegalFunction) }),
        }
    }
}

async fn run_modbus_tcp_client(
    args: Args,
    state: Arc<RwLock<AppState>>,
    _tx: mpsc::UnboundedSender<RegCmd>,
) -> Result<()> {
    let host = if args.device == "dev/null" {
        "127.0.0.1".to_string()
    } else {
        args.device.clone()
    };
    let addr = format!("{}:{}", host, args.tcp_port);
    let stream = tokio::net::TcpStream::connect(&addr)
        .await
        .context("连接 TCP 失败")?;
    let mut ctx = tokio_modbus::client::tcp::attach_slave(stream, Slave(args.unit));

    let tick = Duration::from_millis(args.client_tick_ms);
    let mut interval = tokio::time::interval(tick);

    loop {
        interval.tick().await;

        let once_read_cnt: usize = 120;
        let mut offset: usize = 0;

        while offset < args.holding_count {
            let cnt = (args.holding_count - offset).min(once_read_cnt);
            let addr = offset as u16;

            match ctx.read_holding_registers(addr, cnt as u16).await {
                Ok(rsp) => match rsp {
                    Ok(values) => {
                        let mut s = state.write().await;
                        let end = (offset + values.len()).min(s.holding.len());
                        let write_len = end.saturating_sub(offset);
                        if write_len > 0 {
                            s.holding[offset..offset + write_len]
                                .copy_from_slice(&values[..write_len]);
                        }
                        offset += cnt;
                    }
                    Err(e) => {
                        return Err(anyhow!("Modbus 异常响应: {e:?}"));
                    }
                },
                Err(e) => {
                    return Err(anyhow!("TCP 客户端读取失败: {e}"));
                }
            }
        }
    }
}

//各模式分支函数
async fn run_modbus_tcp_server(
    args: Args,
    _state: Arc<RwLock<AppState>>,
    tx: mpsc::UnboundedSender<RegCmd>,
) -> Result<()> {
    let listener = TcpListener::bind(format!("0.0.0.0:{}", args.tcp_port))
        .await
        .context("打开 Modbus TCP 端口")?;

    let service = HoldingService {
        tx,
        holding_len: args.holding_count,
        unit: args.unit,
    };

    let server = server::tcp::Server::new(listener);

    let abort_signal = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    let on_connected = move |stream, socket_addr| {
        let s = service.clone();
        async move { accept_tcp_connection(stream, socket_addr, move |_sa| Ok(Some(s.clone()))) }
    };

    let on_process_error = |e: std::io::Error| {
        eprintln!("Modbus TCP connection processing error: {e}");
    };

    server
        .serve_until(&on_connected, on_process_error, abort_signal)
        .await
        .map_err(|e: std::io::Error| anyhow!(e))
        .context("Modbus TCP server 运行失败")?;

    Ok(())
}

async fn run_modbus_rtu_client(
    args: Args,
    state: Arc<RwLock<AppState>>,
    _tx: mpsc::UnboundedSender<RegCmd>,
) -> Result<()> {
    let parity = parse_parity(&args.parity)?;
    let flow = parse_flow(&args.flow)?;
    let databits = parse_databits(args.databits)?;
    let stopbits = parse_stopbits(args.stopbits)?;
    let slave = Slave(args.unit);

    let builder = tokio_serial::new(args.device.clone(), args.baudrate)
        .parity(parity)
        .flow_control(flow)
        .data_bits(databits)
        .stop_bits(stopbits);

    let port = builder
        .open_native_async()
        .context("open serial 正在打开串口")?;

    let mut ctx = tokio_modbus::client::rtu::attach_slave(port, slave);

    let tick = Duration::from_millis(args.client_tick_ms);
    let mut interval = tokio::time::interval(tick);

    loop {
        interval.tick().await;
        match ctx
            .read_holding_registers(0, args.holding_count as u16)
            .await
        {
            Ok(rsp) => match rsp {
                Ok(values) => {
                    let mut s = state.write().await;
                    let len = values.len().min(s.holding.len());
                    s.holding[..len].copy_from_slice(&values[..len]);
                }
                Err(e) => {
                    return Err(anyhow!("Modbus 异常响应: {e:?}"));
                }
            },
            Err(e) => {
                return Err(anyhow!("RTU 客户端读取失败: {e}"));
            }
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

    let port = builder
        .open_native_async()
        .context("打开 Modbus RTU 串口")?;

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

                            let t = Table::new(rows, [Constraint::Length(18), Constraint::Length(18), Constraint::Min(10)])
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

#[tokio::main]
async fn main() -> Result<()> {
    let mut args = Args::parse();

    if let Some(profile_name) = &args.profile {
        let config_str = std::fs::read_to_string(&args.config)
            .with_context(|| format!("无法读取配置文件: {}", args.config))?;

        let configs = toml::from_str::<HashMap<String, Args>>(&config_str)
            .with_context(|| format!("配置文件 {} 解析失败", args.config))?;

        if let Some(profile_args) = configs.get(profile_name) {
            args = profile_args.clone();
        } else {
            anyhow::bail!(
                "警告: 配置文件 {} 中找不到预设槽位 '{}'",
                args.config,
                profile_name
            );
        }
    }

    let main_mode = parse_mainmode(&args.main_mode)?;

    let holding = vec![0u16; args.holding_count];

    let mut holding_label = vec!["".to_string(); args.holding_count];
    args.labels.iter().for_each(|(k, v)| {
        if let Ok(idx) = k.parse::<usize>() {
            if idx < holding_label.len() {
                holding_label[idx] = v.clone();
            }
        }
    });

    let state = Arc::new(RwLock::new(AppState {
        holding,
        holding_label,
    }));

    let server_status: Arc<RwLock<Option<String>>> = Arc::new(RwLock::new(None));

    //生产者-消费者模式的寄存器访问通道，Modbus 服务端/客户端任务通过发送命令来访问寄存器数据，UI 任务通过共享状态展示数据
    let (tx, rx) = mpsc::unbounded_channel::<RegCmd>();
    tokio::spawn(reg_worker_loop(Arc::clone(&state), args.holding_count, rx));

    let inner_args = args.clone();
    let inner_state = Arc::clone(&state);
    let inner_tx = tx.clone();
    let inner_status_bg = Arc::clone(&server_status);
    let inner_task = tokio::spawn(async move {
        let r: Result<()> = match main_mode {
            MainMode::RTUServer => run_modbus_rtu_server(inner_args, inner_state, inner_tx).await,
            MainMode::RTUClient => run_modbus_rtu_client(inner_args, inner_state, inner_tx).await,
            MainMode::TcpServer => run_modbus_tcp_server(inner_args, inner_state, inner_tx).await,
            MainMode::TcpClient => run_modbus_tcp_client(inner_args, inner_state, inner_tx).await,
        };

        if let Err(e) = &r {
            *inner_status_bg.write().await = Some(format!("{e:#}"));
        }

        r
    });

    let ui_res = run_ui(Arc::clone(&state), tx, args, server_status).await;

    inner_task.abort();
    let _ = inner_task.await;

    ui_res
}
