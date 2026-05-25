use anyhow::{anyhow, Context, Result};
use clap::{Parser, ValueEnum};
use rand::SeedableRng;
mod modbus;
use std::{collections::HashMap, sync::Arc, time::Instant};
use tokio::sync::{mpsc, RwLock};
use tokio_serial::{DataBits, FlowControl, Parity, StopBits};
mod ui;
use crate::ui::MenuSelection;
use modbus::*;
use serde;
use ui::*;

#[macro_use]
extern crate rust_i18n;

i18n!("locales");

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

    /// 线圈数量（功能码 0x01/0x05/0x0F）
    #[arg(long, default_value_t = 1024)]
    coil_count: usize,

    /// 离散输入数量（功能码 0x02，只读）
    #[arg(long, default_value_t = 1024)]
    discrete_count: usize,

    /// 输入寄存器数量（功能码 0x04，只读）
    #[arg(long, default_value_t = 1024)]
    input_count: usize,

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

    /// 界面语言 (zh-CN 或 en)
    #[arg(long, default_value = "zh-CN")]
    #[serde(skip)]
    lang: String,
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
            coil_count: 512,
            discrete_count: 512,
            input_count: 512,
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
            lang: "zh-CN".into(),
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

/// 单次帧记录（用于监听历史）
#[derive(Clone, Debug)]
pub struct FrameRecord {
    pub timestamp: Instant,
    pub human_time: String, // 可读时间 HH:MM:SS.fff
    pub func_code: u8,
    pub func_name: String,
    pub addr: u16,
    pub values: Vec<u16>,
    pub is_tcp: bool,
    pub is_request: bool, // true=请求, false=响应
    pub unit: u8,         // 单元标识符（RTU 时也作为从站地址）
}

/// 寄存器值变化方向
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum ChangeDirection {
    Up,
    Down,
}

/// 寄存器值变化模式
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum RegChangePattern {
    /// 随机变化（原默认行为）
    Random,
    /// 上下循环：0 → 65535 → 0
    UpDown,
    /// 正弦波
    Sine,
    /// 方波
    Square,
    /// 三角波
    Triangle,
}

impl std::fmt::Display for RegChangePattern {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RegChangePattern::Random => write!(f, "随机"),
            RegChangePattern::UpDown => write!(f, "上下"),
            RegChangePattern::Sine => write!(f, "正弦"),
            RegChangePattern::Square => write!(f, "方波"),
            RegChangePattern::Triangle => write!(f, "三角"),
        }
    }
}

impl std::fmt::Display for ChangeDirection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ChangeDirection::Up => write!(f, "↑"),
            ChangeDirection::Down => write!(f, "↓"),
        }
    }
}

/// 寄存器值变化记录
#[derive(Clone, Debug)]
pub struct RegChangeRecord {
    pub addr: usize,
    pub old_value: u16,
    pub new_value: u16,
    pub direction: ChangeDirection,
    pub human_time: String,
}

/// 静默监听统计数据
#[derive(Clone, Debug)]
pub struct MonitorStats {
    /// 历史记录（环状缓冲区最大 500 条）
    pub history: Vec<FrameRecord>,
    /// 各功能码出现次数
    pub func_count: HashMap<u8, usize>,
    /// 各地址出现次数
    pub addr_count: HashMap<u16, usize>,
    /// 总帧数
    pub total_frames: usize,
}

impl Default for MonitorStats {
    fn default() -> Self {
        Self {
            history: Vec::with_capacity(500),
            func_count: HashMap::new(),
            addr_count: HashMap::new(),
            total_frames: 0,
        }
    }
}

pub struct AppState {
    holding: Vec<u16>,
    holding_label: Vec<String>,
    /// 线圈状态（功能码 0x01/0x05/0x0F）
    pub coils: Vec<bool>,
    /// 离散输入状态（功能码 0x02，只读）
    pub discrete: Vec<bool>,
    /// 输入寄存器状态（功能码 0x04，只读）
    pub input_registers: Vec<u16>,
    pub last_frame: Option<FrameInfo>,
    pub is_tcp: bool,
    /// 监听统计
    pub monitor: MonitorStats,
    /// 稳定性测试进行中
    pub stability_test_running: bool,
    /// 稳定性测试统计 (总周期, 成功, 失败)
    pub stability_stats: (u64, u64, u64),
    /// 寄存器变化历史 (环状缓冲区)
    pub reg_change_history: Vec<RegChangeRecord>,
    /// 寄存器当前值是否刚发生过变化（用于表格高亮）
    pub reg_just_changed: Vec<bool>,
    /// 寄存器变化方向
    pub reg_change_direction: Vec<ChangeDirection>,
    /// 每个寄存器的值变化开启状态（true=开启该寄存器的值变化模拟）
    pub holding_change_enabled: Vec<bool>,
    pub input_change_enabled: Vec<bool>,
    /// 每个寄存器的值变化模式（索引对应 holding 和 input 寄存器）
    pub holding_change_patterns: Vec<RegChangePattern>,
    pub input_change_patterns: Vec<RegChangePattern>,
    /// 波形模式频率（Hz），仅对 Sine/Square/Triangle 有效
    pub holding_pattern_freqs: Vec<f64>,
    pub input_pattern_freqs: Vec<f64>,
    /// 相位累加器（用于波形/上下计数追踪）
    pub holding_pattern_phases: Vec<f64>,
    pub input_pattern_phases: Vec<f64>,
    /// 客户端模式下各寄存器类型的读取启用状态 [holding, coils, discrete, input]
    pub read_enabled: [bool; 4],
    /// 从设备扫描结果 (slave_id, Option<register_value>)
    pub slave_scan_result: Option<Vec<(u8, Option<u16>)>>,
    /// 从设备扫描正在进行
    pub slave_scan_running: bool,
    /// 每个寄存器的值变化条形图历史（每地址最多 20 个采样值）
    pub reg_bar_history: Vec<Vec<u16>>,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            holding: Vec::new(),
            holding_label: Vec::new(),
            coils: Vec::new(),
            discrete: Vec::new(),
            input_registers: Vec::new(),
            last_frame: None,
            is_tcp: true,
            monitor: MonitorStats::default(),
            stability_test_running: false,
            stability_stats: (0, 0, 0),
            reg_change_history: Vec::new(),
            reg_just_changed: Vec::new(),
            reg_change_direction: Vec::new(),
            holding_change_enabled: Vec::new(),
            input_change_enabled: Vec::new(),
            holding_change_patterns: Vec::new(),
            input_change_patterns: Vec::new(),
            holding_pattern_freqs: Vec::new(),
            input_pattern_freqs: Vec::new(),
            holding_pattern_phases: Vec::new(),
            input_pattern_phases: Vec::new(),
            read_enabled: [true, false, false, false],
            slave_scan_result: None,
            slave_scan_running: false,
            reg_bar_history: Vec::new(),
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum MainMode {
    TcpServer,
    TcpClient,
    RTUServer,
    RTUClient,
    Monitor,
}

/// 向监听统计中添加一条帧记录
pub fn record_frame(monitor: &mut MonitorStats, fi: &FrameInfo) {
    const MAX_HISTORY: usize = 500;
    let now = std::time::SystemTime::now();
    let human_time = format_system_time(now);
    let record = FrameRecord {
        timestamp: Instant::now(),
        human_time,
        func_code: fi.func_code,
        func_name: fi.func_name.clone(),
        addr: fi.addr,
        values: fi.values.clone(),
        is_tcp: fi.is_tcp,
        is_request: fi.is_request,
        unit: fi.unit,
    };
    if monitor.history.len() >= MAX_HISTORY {
        monitor.history.remove(0);
    }
    monitor.history.push(record);
    *monitor.func_count.entry(fi.func_code).or_insert(0) += 1;
    *monitor.addr_count.entry(fi.addr).or_insert(0) += 1;
    monitor.total_frames += 1;
}

fn format_system_time(t: std::time::SystemTime) -> String {
    match t.duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => {
            let secs = d.as_secs();
            let millis = d.subsec_millis();
            let hours = (secs / 3600) % 24;
            let minutes = (secs / 60) % 60;
            let seconds = secs % 60;
            format!("{:02}:{:02}:{:02}.{:03}", hours, minutes, seconds, millis)
        }
        Err(_) => "??:??:??.???".to_string(),
    }
}

pub const BAR_HISTORY_SLOTS: usize = 20;

/// 记录寄存器值变化
pub fn record_reg_change(state: &mut AppState, addr: usize, old_value: u16, new_value: u16) {
    const MAX_CHANGES: usize = 500;
    let now = std::time::SystemTime::now();
    let human_time = format_system_time(now);
    let direction = if new_value > old_value {
        ChangeDirection::Up
    } else if new_value < old_value {
        ChangeDirection::Down
    } else {
        return; // 没变化，不记录
    };
    let record = RegChangeRecord {
        addr,
        old_value,
        new_value,
        direction: direction.clone(),
        human_time,
    };
    if state.reg_change_history.len() >= MAX_CHANGES {
        state.reg_change_history.remove(0);
    }
    state.reg_change_history.push(record);
    if addr < state.reg_just_changed.len() {
        state.reg_just_changed[addr] = true;
        state.reg_change_direction[addr] = direction;
    }
    // 更新条形图历史
    while state.reg_bar_history.len() <= addr {
        state
            .reg_bar_history
            .push(Vec::with_capacity(BAR_HISTORY_SLOTS));
    }
    let bar = &mut state.reg_bar_history[addr];
    if bar.len() >= BAR_HISTORY_SLOTS {
        bar.remove(0);
    }
    bar.push(new_value);
}

/// 计算单个寄存器的值（基于其变化模式）
fn compute_pattern_value(
    pattern: RegChangePattern,
    freq: f64,
    phase: &mut f64,
    _elapsed: f64,
    tick_secs: f64,
    current: u16,
    is_up: &mut bool,
) -> u16 {
    match pattern {
        RegChangePattern::Random => {
            use rand::Rng;
            let mut rng = rand::rngs::StdRng::from_entropy();
            let delta: u16 = rng.gen_range(1..=50);
            if rng.gen_bool(0.5) {
                current.saturating_add(delta)
            } else {
                current.saturating_sub(delta)
            }
        }
        RegChangePattern::UpDown => {
            const MAX_VAL: u16 = 65535;
            if *is_up {
                let next = current.saturating_add(1);
                if next == MAX_VAL {
                    *is_up = false;
                    next
                } else {
                    next
                }
            } else {
                let next = current.saturating_sub(1);
                if next == 0 {
                    *is_up = true;
                    next
                } else {
                    next
                }
            }
        }
        RegChangePattern::Sine => {
            *phase += 2.0 * std::f64::consts::PI * freq * tick_secs;
            if *phase > 2.0 * std::f64::consts::PI * 1000.0 {
                *phase -= 2.0 * std::f64::consts::PI * 1000.0;
            }
            let sin_val = phase.sin();
            // Map -1..1 to 0..65535
            ((sin_val + 1.0) * 32767.5) as u16
        }
        RegChangePattern::Square => {
            *phase += 2.0 * std::f64::consts::PI * freq * tick_secs;
            if *phase > 2.0 * std::f64::consts::PI * 1000.0 {
                *phase -= 2.0 * std::f64::consts::PI * 1000.0;
            }
            if phase.sin() >= 0.0 {
                65535
            } else {
                0
            }
        }
        RegChangePattern::Triangle => {
            *phase += freq * tick_secs;
            if *phase > 1000.0 {
                *phase -= 1000.0;
            }
            let t = *phase;
            let val = 2.0 * (t - (t + 0.5).floor()).abs();
            (val * 65535.0) as u16
        }
    }
}

/// 模拟寄存器值变化（使用每个寄存器独立的模式）
pub async fn run_register_simulator(
    state: Arc<RwLock<AppState>>,
    _holding_count: usize,
    tick_ms: u64,
) {
    let tick_secs = tick_ms as f64 / 1000.0;
    let mut interval = tokio::time::interval(std::time::Duration::from_millis(tick_ms));
    loop {
        interval.tick().await;

        let mut s = state.write().await;

        // 确保 pattern/phase/freq/enabled 向量长度 >= 寄存器数量
        while s.holding_change_enabled.len() < s.holding.len() {
            s.holding_change_enabled.push(false);
        }
        while s.input_change_enabled.len() < s.input_registers.len() {
            s.input_change_enabled.push(false);
        }
        while s.holding_change_patterns.len() < s.holding.len() {
            s.holding_change_patterns.push(RegChangePattern::Random);
            s.holding_pattern_freqs.push(1.0);
            s.holding_pattern_phases.push(0.0);
        }
        while s.input_change_patterns.len() < s.input_registers.len() {
            s.input_change_patterns.push(RegChangePattern::Random);
            s.input_pattern_freqs.push(1.0);
            s.input_pattern_phases.push(0.0);
        }
        // 确保 reg_change_direction 向量足够长
        while s.reg_change_direction.len() < s.holding.len() + s.input_registers.len() {
            s.reg_change_direction.push(ChangeDirection::Up);
        }
        while s.reg_just_changed.len() < s.holding.len() + s.input_registers.len() {
            s.reg_just_changed.push(false);
        }
        while s.reg_bar_history.len() < s.holding.len() + s.input_registers.len() {
            s.reg_bar_history
                .push(Vec::with_capacity(BAR_HISTORY_SLOTS));
        }

        // --- 更新每个保持寄存器 ---
        for addr in 0..s.holding.len() {
            if addr >= s.holding_change_enabled.len() || !s.holding_change_enabled[addr] {
                continue;
            }
            if addr >= s.holding_change_patterns.len() {
                break;
            }
            let pattern = s.holding_change_patterns[addr];
            let freq = if addr < s.holding_pattern_freqs.len() {
                s.holding_pattern_freqs[addr]
            } else {
                1.0
            };
            let mut phase = s.holding_pattern_phases.get(addr).copied().unwrap_or(0.0);
            let old = s.holding[addr];
            let mut is_up = s
                .reg_change_direction
                .get(addr)
                .copied()
                .unwrap_or(ChangeDirection::Up)
                == ChangeDirection::Up;

            // 随机模式按原设计有概率不触发
            if pattern == RegChangePattern::Random {
                use rand::Rng;
                if rand::rngs::StdRng::from_entropy().gen_bool(0.67) {
                    continue; // 67% 概率跳过（保持与原来 1~3 个变化大致相当）
                }
            }

            let new =
                compute_pattern_value(pattern, freq, &mut phase, 0.0, tick_secs, old, &mut is_up);
            if addr < s.holding_pattern_phases.len() {
                s.holding_pattern_phases[addr] = phase;
            }
            if addr < s.reg_change_direction.len() {
                s.reg_change_direction[addr] = if is_up {
                    ChangeDirection::Up
                } else {
                    ChangeDirection::Down
                };
            }
            if old != new {
                s.holding[addr] = new;
                record_reg_change(&mut s, addr, old, new);
            }
        }

        // --- 更新每个输入寄存器 ---
        for addr in 0..s.input_registers.len() {
            if addr >= s.input_change_enabled.len() || !s.input_change_enabled[addr] {
                continue;
            }
            if addr >= s.input_change_patterns.len() {
                break;
            }
            let pattern = s.input_change_patterns[addr];
            let freq = if addr < s.input_pattern_freqs.len() {
                s.input_pattern_freqs[addr]
            } else {
                1.0
            };
            let mut phase = s.input_pattern_phases.get(addr).copied().unwrap_or(0.0);
            let offset = s.holding.len();
            let old = s.input_registers[addr];
            let mut is_up = s
                .reg_change_direction
                .get(offset + addr)
                .copied()
                .unwrap_or(ChangeDirection::Up)
                == ChangeDirection::Up;

            if pattern == RegChangePattern::Random {
                use rand::Rng;
                if rand::rngs::StdRng::from_entropy().gen_bool(0.67) {
                    continue;
                }
            }

            let new =
                compute_pattern_value(pattern, freq, &mut phase, 0.0, tick_secs, old, &mut is_up);
            if addr < s.input_pattern_phases.len() {
                s.input_pattern_phases[addr] = phase;
            }
            if offset + addr < s.reg_change_direction.len() {
                s.reg_change_direction[offset + addr] = if is_up {
                    ChangeDirection::Up
                } else {
                    ChangeDirection::Down
                };
            }
            if old != new {
                s.input_registers[addr] = new;
            }
        }
    }
}

fn parse_mainmode(s: &str) -> Result<MainMode> {
    match s.to_ascii_lowercase().as_str() {
        "ts" | "tcp-server" => Ok(MainMode::TcpServer),
        "tc" | "tcp-client" => Ok(MainMode::TcpClient),
        "rs" | "rtu-server" => Ok(MainMode::RTUServer),
        "rc" | "rtu-client" => Ok(MainMode::RTUClient),
        "mo" | "monitor" => Ok(MainMode::Monitor),
        _ => Err(anyhow!(t!("main.invalid_main_mode", mode = s))),
    }
}

fn parse_parity(s: &str) -> Result<Parity> {
    match s.to_ascii_lowercase().as_str() {
        "n" | "none" => Ok(Parity::None),
        "e" | "even" => Ok(Parity::Even),
        "o" | "odd" => Ok(Parity::Odd),
        _ => Err(anyhow!(t!("main.invalid_parity", value = s))),
    }
}
fn parse_flow(s: &str) -> Result<FlowControl> {
    match s.to_ascii_lowercase().as_str() {
        "none" => Ok(FlowControl::None),
        "hard" | "hw" | "rtscts" => Ok(FlowControl::Hardware),
        "soft" | "sw" | "xonxoff" => Ok(FlowControl::Software),
        _ => Err(anyhow!(t!("main.invalid_flow", value = s))),
    }
}
fn parse_databits(v: u8) -> Result<DataBits> {
    match v {
        5 => Ok(DataBits::Five),
        6 => Ok(DataBits::Six),
        7 => Ok(DataBits::Seven),
        8 => Ok(DataBits::Eight),
        _ => Err(anyhow!(t!("main.invalid_databits", value = v))),
    }
}
fn parse_stopbits(v: u8) -> Result<StopBits> {
    match v {
        1 => Ok(StopBits::One),
        2 => Ok(StopBits::Two),
        _ => Err(anyhow!(t!("main.invalid_stopbits", value = v))),
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
        return Err(anyhow!(t!("main.parse_empty")));
    }
    //允许通过前缀强制定义输入类型
    if let Some(rest) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        return u16::from_str_radix(rest, 16).context(t!("main.parse_hex_prefix"));
    }
    if let Some(rest) = t.strip_prefix("0b").or_else(|| t.strip_prefix("0B")) {
        return u16::from_str_radix(rest, 2).context(t!("main.parse_bin_prefix"));
    }
    match base {
        DisplayBase::Dec => t.parse::<u16>().context(t!("main.parse_dec")),
        DisplayBase::Hex => u16::from_str_radix(t, 16).context(t!("main.parse_hex")),
        DisplayBase::Bin => u16::from_str_radix(t, 2).context(t!("main.parse_bin")),
    }
}

/// 根据 MenuSelection 加载对应配置并解包为 (MainMode, Args)
fn resolve_selection(config_path: &str, sel: &MenuSelection) -> Result<(MainMode, Args)> {
    let main_mode = sel.main_mode;

    let mut args = if let Some(ref profile_name) = sel.profile_name {
        // 加载指定配置
        let config_str = std::fs::read_to_string(config_path)
            .with_context(|| t!("main.read_config_fail", path = config_path))?;
        let configs = toml::from_str::<HashMap<String, Args>>(&config_str)
            .with_context(|| t!("main.parse_config_fail", path = config_path))?;
        configs
            .get(profile_name)
            .cloned()
            .ok_or_else(|| anyhow!(t!("main.profile_missing", name = profile_name)))?
    } else {
        Args::default()
    };

    // 覆盖 main_mode 为菜单选择的模式
    args.main_mode = match main_mode {
        MainMode::TcpServer => "tcp-server".into(),
        MainMode::TcpClient => "tcp-client".into(),
        MainMode::RTUServer => "rtu-server".into(),
        MainMode::RTUClient => "rtu-client".into(),
        MainMode::Monitor => "monitor".into(),
    };

    Ok((main_mode, args))
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli_args = Args::parse();

    // 设置界面语言
    rust_i18n::set_locale(&cli_args.lang);

    // 配置路径（Clone 以避免 move）
    let config_path_base = cli_args.config.clone();

    // 如果 CLI 指定了 --profile，直接使用（跳过菜单）
    let (main_mode, args) = if let Some(profile_name) = &cli_args.profile {
        let config_str = std::fs::read_to_string(&cli_args.config)
            .with_context(|| t!("main.read_config_fail", path = &cli_args.config))?;
        let configs = toml::from_str::<HashMap<String, Args>>(&config_str)
            .with_context(|| t!("main.parse_config_fail", path = &cli_args.config))?;
        if let Some(profile_args) = configs.get(profile_name) {
            let mut args = profile_args.clone();
            let main_mode = parse_mainmode(&args.main_mode)?;
            args.config = config_path_base;
            (main_mode, args)
        } else {
            anyhow::bail!(t!("main.profile_not_found", name = profile_name));
        }
    } else {
        // 加载配置列表
        let profiles = load_profile_list(&cli_args.config);

        // 显示菜单
        let sel = run_menu(&cli_args.config, profiles).await?;
        if sel.quit {
            return Ok(());
        }
        // 用户选择的配置解析
        resolve_selection(&cli_args.config, &sel)?
    };

    let binding_count = args.holding_count;
    let max_count = binding_count
        .max(args.coil_count)
        .max(args.discrete_count)
        .max(args.input_count);
    let mut holding_label = vec!["".to_string(); binding_count];
    args.labels.iter().for_each(|(k, v)| {
        if let Ok(idx) = k.parse::<usize>() {
            if idx < holding_label.len() {
                holding_label[idx] = v.clone();
            }
        }
    });
    let state = Arc::new(RwLock::new(AppState {
        holding: vec![0u16; binding_count],
        holding_label,
        coils: vec![false; args.coil_count],
        discrete: vec![false; args.discrete_count],
        input_registers: vec![0u16; args.input_count],
        last_frame: None,
        is_tcp: matches!(main_mode, MainMode::TcpServer | MainMode::TcpClient),
        monitor: MonitorStats::default(),
        stability_test_running: false,
        stability_stats: (0, 0, 0),
        reg_change_history: Vec::new(),
        reg_just_changed: vec![false; max_count],
        reg_change_direction: vec![ChangeDirection::Up; max_count],
        holding_change_enabled: vec![false; binding_count],
        input_change_enabled: vec![false; args.input_count],
        holding_change_patterns: vec![RegChangePattern::Random; binding_count],
        input_change_patterns: vec![RegChangePattern::Random; args.input_count],
        holding_pattern_freqs: vec![1.0; binding_count],
        input_pattern_freqs: vec![1.0; args.input_count],
        holding_pattern_phases: vec![0.0; binding_count],
        input_pattern_phases: vec![0.0; args.input_count],
        read_enabled: [true, false, false, false],
        slave_scan_result: None,
        slave_scan_running: false,
        reg_bar_history: vec![vec![]; max_count],
    }));

    let server_status: Arc<RwLock<Option<String>>> = Arc::new(RwLock::new(None));

    //生产者-消费者模式的寄存器访问通道，Modbus 服务端/客户端任务通过发送命令来访问寄存器数据，UI 任务通过共享状态展示数据
    let (server_tx, server_rx) = mpsc::unbounded_channel::<RegCmd>();
    tokio::spawn(reg_worker_loop(Arc::clone(&state), server_rx));

    // 服务端模式下启动寄存器值模拟器（随机变化）
    let is_server = matches!(main_mode, MainMode::TcpServer | MainMode::RTUServer);
    if is_server {
        let sim_state = Arc::clone(&state);
        let hc = args.holding_count;
        tokio::spawn(async move {
            run_register_simulator(sim_state, hc, /* 每 500ms 变化一次 */ 500).await;
        });
    }

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
            MainMode::Monitor => {
                // Monitor 模式不需要 modbus 后端，仅保持运行
                futures::future::pending::<Result<()>>().await
            }
        };

        if let Err(e) = &r {
            *inner_status_bg.write().await = Some(format!("{e:#}"));
        }

        r
    });
    let ui_res;
    // 加载配置列表（供监听模式使用）
    let config_path = cli_args.config.clone();
    let profiles = load_profile_list(&config_path);
    if main_mode == MainMode::TcpClient || main_mode == MainMode::RTUClient {
        ui_res = run_ui(
            Arc::clone(&state),
            client_tx,
            args,
            server_status,
            config_path,
            profiles,
        )
        .await;
    } else {
        ui_res = run_ui(
            Arc::clone(&state),
            server_tx,
            args,
            server_status,
            config_path,
            profiles,
        )
        .await;
    }

    inner_task.abort();
    let _ = inner_task.await;

    ui_res
}
