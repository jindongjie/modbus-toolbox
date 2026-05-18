use anyhow::{anyhow, Context, Result};
use clap::{Parser, ValueEnum};
mod modbus;
use std::{collections::HashMap, sync::Arc};
use tokio::sync::{mpsc, RwLock};
use tokio_serial::{DataBits, FlowControl, Parity, StopBits};
mod ui;
use crate::ui::MenuSelection;
use modbus::*;
use serde;
use ui::*;

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

#[derive(Clone, Debug, Default)]
pub struct FrameInfo {
    pub is_tcp: bool,
    pub unit: u8,
    pub func_code: u8,
    pub func_name: String,
    pub addr: u16,
    pub values: Vec<u16>,
    pub is_request: bool,
}

#[derive(Default)]
struct AppState {
    holding: Vec<u16>,
    holding_label: Vec<String>,
    pub last_frame: Option<FrameInfo>,
    pub is_tcp: bool,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
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

/// 根据 MenuSelection 加载对应配置并解包为 (MainMode, Args)
fn resolve_selection(config_path: &str, sel: &MenuSelection) -> Result<(MainMode, Args)> {
    let main_mode = sel.main_mode;

    let mut args = if let Some(ref profile_name) = sel.profile_name {
        // 加载指定配置
        let config_str = std::fs::read_to_string(config_path)
            .with_context(|| format!("无法读取配置文件: {}", config_path))?;
        let configs = toml::from_str::<HashMap<String, Args>>(&config_str)
            .with_context(|| format!("配置文件 {} 解析失败", config_path))?;
        configs
            .get(profile_name)
            .cloned()
            .ok_or_else(|| anyhow!("配置文件中找不到 '{}'", profile_name))?
    } else {
        Args::default()
    };

    // 覆盖 main_mode 为菜单选择的模式
    args.main_mode = match main_mode {
        MainMode::TcpServer => "tcp-server".into(),
        MainMode::TcpClient => "tcp-client".into(),
        MainMode::RTUServer => "rtu-server".into(),
        MainMode::RTUClient => "rtu-client".into(),
    };

    Ok((main_mode, args))
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli_args = Args::parse();

    // 如果 CLI 指定了 --profile，直接使用（跳过菜单）
    let (main_mode, args) = if let Some(profile_name) = &cli_args.profile {
        let config_str = std::fs::read_to_string(&cli_args.config)
            .with_context(|| format!("无法读取配置文件: {}", cli_args.config))?;
        let configs = toml::from_str::<HashMap<String, Args>>(&config_str)
            .with_context(|| format!("配置文件 {} 解析失败", cli_args.config))?;
        if let Some(profile_args) = configs.get(profile_name) {
            let mut args = profile_args.clone();
            let main_mode = parse_mainmode(&args.main_mode)?;
            args.config = cli_args.config;
            (main_mode, args)
        } else {
            anyhow::bail!(
                "配置文件中找不到预设槽位 '{}'",
                profile_name
            );
        }
    } else {
        // 加载配置列表
        let profiles = load_profile_list(&cli_args.config);

        // 显示菜单
        let sel = run_menu(&cli_args.config, profiles).await?;
        // 用户按 q 退出时的处理
        resolve_selection(&cli_args.config, &sel)?
    };

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
        last_frame: None,
        is_tcp: matches!(main_mode, MainMode::TcpServer | MainMode::TcpClient),
    }));

    let server_status: Arc<RwLock<Option<String>>> = Arc::new(RwLock::new(None));

    //生产者-消费者模式的寄存器访问通道，Modbus 服务端/客户端任务通过发送命令来访问寄存器数据，UI 任务通过共享状态展示数据
    let (server_tx, server_rx) = mpsc::unbounded_channel::<RegCmd>();
    tokio::spawn(reg_worker_loop(
        Arc::clone(&state),
        args.holding_count,
        server_rx,
    ));

    let (client_tx, inner_rx) = mpsc::unbounded_channel::<RegCmd>();

    let inner_args = args.clone();
    let inner_state = Arc::clone(&state);
    let inner_tx = server_tx.clone();
    let inner_status_bg = Arc::clone(&server_status);
    let inner_task = tokio::spawn(async move {
        let r: Result<()> = match main_mode {
            MainMode::RTUServer => run_modbus_rtu_server(inner_args, inner_state, inner_tx).await,
            MainMode::RTUClient => run_modbus_rtu_client(inner_args, inner_state, inner_rx).await,
            MainMode::TcpServer => run_modbus_tcp_server(inner_args, inner_state, inner_tx).await,
            MainMode::TcpClient => run_modbus_tcp_client(inner_args, inner_state, inner_rx).await,
        };

        if let Err(e) = &r {
            *inner_status_bg.write().await = Some(format!("{e:#}"));
        }

        r
    });
    let ui_res;
    if main_mode == MainMode::TcpClient || main_mode == MainMode::RTUClient {
        ui_res = run_ui(Arc::clone(&state), client_tx, args, server_status).await;
    } else {
        ui_res = run_ui(Arc::clone(&state), server_tx, args, server_status).await;
    }

    inner_task.abort();
    let _ = inner_task.await;

    ui_res
}
