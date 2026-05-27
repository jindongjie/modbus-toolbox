use anyhow::{anyhow, Context, Result};
use clap::Parser;
mod modbus;
use std::{collections::HashMap, sync::Arc, time::Instant};
use tokio::sync::{mpsc, RwLock};
use tokio_serial::{DataBits, FlowControl, Parity, StopBits};
mod ui;
use crate::ui::MenuSelection;
use modbus::*;
use ui::*;

#[macro_use]
extern crate rust_i18n;

i18n!("locales");

/// 寄存器数据类型
#[derive(Copy, Clone, Debug, PartialEq, Default, serde::Deserialize, serde::Serialize)]
pub enum RegDataType {
    #[default]
    Uint,
    Int,
    Float,
    Hex,
    Binary,
    Ascii,
}

/// 寄存器数据位宽
#[derive(Copy, Clone, Debug, PartialEq, Default, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RegDataWidth {
    #[default]
    Bits16,
    Bits32,
    Bits64,
    Bits128,
}

/// 寄存器数据解释格式 = 类型 + 位宽
#[derive(Copy, Clone, Debug, PartialEq, Default, serde::Deserialize, serde::Serialize)]
pub struct RegDataFormat {
    pub data_type: RegDataType,
    pub width: RegDataWidth,
}

impl RegDataFormat {
    /// 此格式需要的连续 16 位寄存器数量
    pub fn regs_needed(self) -> usize {
        match self.width {
            RegDataWidth::Bits16 => 1,
            RegDataWidth::Bits32 => 2,
            RegDataWidth::Bits64 => 4,
            RegDataWidth::Bits128 => 8,
        }
    }

    fn type_prefix(self) -> &'static str {
        match self.data_type {
            RegDataType::Uint => "u",
            RegDataType::Int => "i",
            RegDataType::Float => "f",
            RegDataType::Hex => "hex",
            RegDataType::Binary => "bin",
            RegDataType::Ascii => "ascii",
        }
    }

    fn width_suffix(self) -> &'static str {
        match self.width {
            RegDataWidth::Bits16 => "16",
            RegDataWidth::Bits32 => "32",
            RegDataWidth::Bits64 => "64",
            RegDataWidth::Bits128 => "128",
        }
    }

    pub fn short_label(self) -> String {
        match self.data_type {
            RegDataType::Hex | RegDataType::Binary | RegDataType::Ascii => {
                self.type_prefix().to_string()
            }
            _ => format!("{}{}", self.type_prefix(), self.width_suffix()),
        }
    }

    /// 迭代所有数据类型的循环顺序
    pub fn all_types() -> &'static [RegDataType] {
        &[
            RegDataType::Uint,
            RegDataType::Int,
            RegDataType::Float,
            RegDataType::Hex,
            RegDataType::Binary,
            RegDataType::Ascii,
        ]
    }

    /// 循环切换类型：Uint→Int→Float→Hex→Binary→Uint
    pub fn next_type(self) -> RegDataFormat {
        let next = match self.data_type {
            RegDataType::Uint => RegDataType::Int,
            RegDataType::Int => RegDataType::Float,
            RegDataType::Float => RegDataType::Hex,
            RegDataType::Hex => RegDataType::Binary,
            RegDataType::Binary | RegDataType::Ascii => RegDataType::Uint,
        };
        let width = RegDataFormat::sanitize_width(next, self.width);
        RegDataFormat {
            data_type: next,
            width,
        }
    }

    /// 反向循环切换类型：Uint←Binary←Hex←Float←Int←Uint
    pub fn prev_type(self) -> RegDataFormat {
        let prev = match self.data_type {
            RegDataType::Uint => RegDataType::Binary,
            RegDataType::Int => RegDataType::Uint,
            RegDataType::Float => RegDataType::Int,
            RegDataType::Hex => RegDataType::Float,
            RegDataType::Binary | RegDataType::Ascii => RegDataType::Hex,
        };
        let width = RegDataFormat::sanitize_width(prev, self.width);
        RegDataFormat {
            data_type: prev,
            width,
        }
    }

    /// 确保位宽对指定类型有效
    fn sanitize_width(data_type: RegDataType, width: RegDataWidth) -> RegDataWidth {
        match data_type {
            RegDataType::Float => match width {
                RegDataWidth::Bits128 => RegDataWidth::Bits64, // 无 F128
                w => w,
            },
            _ => width,
        }
    }

    /// 循环增加位宽：16→32→64→128→16
    pub fn next_width(self) -> RegDataFormat {
        let w = match self.width {
            RegDataWidth::Bits16 => RegDataWidth::Bits32,
            RegDataWidth::Bits32 => RegDataWidth::Bits64,
            RegDataWidth::Bits64 => RegDataWidth::Bits128,
            RegDataWidth::Bits128 => RegDataWidth::Bits16,
        };
        let width = RegDataFormat::sanitize_width(self.data_type, w);
        RegDataFormat {
            data_type: self.data_type,
            width,
        }
    }

    /// 循环减少位宽：128→64→32→16→128
    pub fn prev_width(self) -> RegDataFormat {
        let w = match self.width {
            RegDataWidth::Bits16 => RegDataWidth::Bits128,
            RegDataWidth::Bits32 => RegDataWidth::Bits16,
            RegDataWidth::Bits64 => RegDataWidth::Bits32,
            RegDataWidth::Bits128 => RegDataWidth::Bits64,
        };
        let width = RegDataFormat::sanitize_width(self.data_type, w);
        RegDataFormat {
            data_type: self.data_type,
            width,
        }
    }

    /// 将指定类型设为 Uint，保持位宽
    pub fn to_uint(self) -> RegDataFormat {
        RegDataFormat {
            data_type: RegDataType::Uint,
            width: self.width,
        }
    }

    /// 将指定类型设为 Int，保持位宽
    pub fn to_int(self) -> RegDataFormat {
        RegDataFormat {
            data_type: RegDataType::Int,
            width: self.width,
        }
    }

    /// 将指定类型设为 Float，保持当前位宽
    pub fn to_float(self) -> RegDataFormat {
        RegDataFormat {
            data_type: RegDataType::Float,
            width: RegDataFormat::sanitize_width(RegDataType::Float, self.width),
        }
    }

    /// 将指定类型设为 Hex，保持当前位宽
    pub fn to_hex(self) -> RegDataFormat {
        RegDataFormat {
            data_type: RegDataType::Hex,
            width: RegDataFormat::sanitize_width(RegDataType::Hex, self.width),
        }
    }

    /// 将指定类型设为 Binary，保持当前位宽
    pub fn to_binary(self) -> RegDataFormat {
        RegDataFormat {
            data_type: RegDataType::Binary,
            width: RegDataFormat::sanitize_width(RegDataType::Binary, self.width),
        }
    }

    /// 将指定类型设为 Ascii，保持当前位宽
    pub fn to_ascii(self) -> RegDataFormat {
        RegDataFormat {
            data_type: RegDataType::Ascii,
            width: RegDataFormat::sanitize_width(RegDataType::Ascii, self.width),
        }
    }
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

    #[arg(long, default_value = "u16")]
    #[serde(default)]
    reg_format: String,

    /// 寄存器备注标签
    #[arg(skip)]
    #[serde(default)]
    labels: HashMap<String, String>,

    /// 寄存器组合配置 (地址 → 数据格式，如 "0" → "i32" 表示地址0开始2个寄存器组合为i32)
    #[arg(skip)]
    #[serde(default)]
    reg_combinations: HashMap<String, String>,

    /// Per-register byte swap (地址 → "true"/"false"，"i:addr" 表示输入寄存器)
    #[arg(skip)]
    #[serde(default)]
    swap_bytes_reg: HashMap<String, String>,

    /// Per-register word swap (地址 → "true"/"false"，"i:addr" 表示输入寄存器)
    #[arg(skip)]
    #[serde(default)]
    swap_words_reg: HashMap<String, String>,

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
            reg_format: "u16".into(),
            labels: HashMap::new(),
            reg_combinations: HashMap::new(),
            swap_bytes_reg: HashMap::new(),
            swap_words_reg: HashMap::new(),
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
    /// 当前寄存器数据解释格式
    pub reg_format: RegDataFormat,
    /// Per-register byte swap for holding registers
    pub holding_swap_bytes: HashMap<usize, bool>,
    /// Per-register word swap for holding registers
    pub holding_swap_words: HashMap<usize, bool>,
    /// Per-register byte swap for input registers
    pub input_swap_bytes: HashMap<usize, bool>,
    /// Per-register word swap for input registers
    pub input_swap_words: HashMap<usize, bool>,
    /// Holding 寄存器组合配置 (primary address → format)
    pub holding_combinations: HashMap<usize, RegDataFormat>,
    /// Input 寄存器组合配置 (primary address → format)
    pub input_combinations: HashMap<usize, RegDataFormat>,
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
            reg_format: RegDataFormat::default(),
            holding_swap_bytes: HashMap::new(),
            holding_swap_words: HashMap::new(),
            input_swap_bytes: HashMap::new(),
            input_swap_words: HashMap::new(),
            holding_combinations: HashMap::new(),
            input_combinations: HashMap::new(),
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum MainMode {
    TcpServer,
    TcpClient,
    RTUServer,
    RTUClient,
    TcpMonitor,
    RtuMonitor,
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

/// CSV logging directory for monitor mode
pub const MONITOR_LOG_DIR: &str = "./monitor";

/// Write CSV header to a new log file
pub fn csv_log_header(path: &std::path::Path) -> std::io::Result<()> {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)?;
    writeln!(
        f,
        "time,direction,protocol,unit,func_code,func_name,addr,values"
    )
}

/// Append a frame record as a CSV row to the log file
pub fn csv_log_append(path: &std::path::Path, rec: &FrameRecord) -> std::io::Result<()> {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    let dir = if rec.is_request { "REQ" } else { "RSP" };
    let proto = if rec.is_tcp { "TCP" } else { "RTU" };
    let values_str: Vec<String> = rec.values.iter().map(|v| v.to_string()).collect();
    writeln!(
        f,
        "{},{},{},{},0x{:02X},{},0x{:04X},\"[{}]\"",
        rec.human_time,
        dir,
        proto,
        rec.unit,
        rec.func_code,
        rec.func_name,
        rec.addr,
        values_str.join(";")
    )
}

/// Generate a CSV log file path based on mode, port/device, and current time.
/// For TCP mode: `tcp-{port}-{time}.csv`
/// For RTU mode: `rtu-{device}-{time}.csv`
pub fn csv_log_path(main_mode: &str, tcp_port: u16, device: &str) -> std::path::PathBuf {
    let now = std::time::SystemTime::now();
    let timestamp = match now.duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => {
            let total_secs = d.as_secs();
            // Use UTC time components directly
            let secs_in_day = total_secs % 86400;
            let h = secs_in_day / 3600;
            let m = (secs_in_day % 3600) / 60;
            let s = secs_in_day % 60;
            // Days since epoch for date calculation
            let days = total_secs / 86400;
            let (year, month, day) = days_to_date(days);
            format!("{:04}{:02}{:02}_{:02}{:02}{:02}", year, month, day, h, m, s)
        }
        Err(_) => "unknown".to_string(),
    };
    let prefix = if main_mode.contains("rtu") {
        // Sanitize device name for use in filename (replace path separators)
        let dev_name = device.replace(['/', '\\'], "_");
        format!("rtu-{}-{}", dev_name, timestamp)
    } else {
        format!("tcp-{}-{}", tcp_port, timestamp)
    };
    std::path::PathBuf::from(MONITOR_LOG_DIR).join(format!("{}.csv", prefix))
}

/// Convert days since Unix epoch to (year, month, day)
fn days_to_date(days: u64) -> (u64, u64, u64) {
    // Civil calendar algorithm from http://howardhinnant.github.io/date_algorithms.html
    let z = days + 719468;
    let era = z / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

/// List all CSV files in the monitor log directory
pub fn list_csv_logs() -> Vec<std::path::PathBuf> {
    let dir = std::path::Path::new(MONITOR_LOG_DIR);
    if !dir.exists() {
        return Vec::new();
    }
    let mut files: Vec<std::path::PathBuf> = std::fs::read_dir(dir)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().map(|e| e == "csv").unwrap_or(false))
        .collect();
    files.sort();
    files.reverse(); // newest first
    files
}

/// Parse a CSV log file back into a vector of FrameRecords
pub fn parse_csv_log(path: &std::path::Path) -> Result<Vec<FrameRecord>> {
    let content = std::fs::read_to_string(path)?;
    let mut records = Vec::new();
    for (i, line) in content.lines().enumerate() {
        if i == 0 {
            continue; // skip header
        }
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        // Parse: time,direction,protocol,unit,func_code,func_name,addr,"[values]"
        // We need to handle the quoted values field
        let parts: Vec<&str> = if let Some(quote_start) = line.find('"') {
            let prefix = &line[..quote_start];
            let suffix = &line[quote_start..];
            let mut p: Vec<&str> = prefix.trim_end_matches(',').split(',').collect();
            p.push(suffix.trim_matches('"').trim_matches('[').trim_matches(']'));
            p
        } else {
            line.split(',').collect()
        };
        if parts.len() < 7 {
            continue;
        }
        let human_time = parts[0].to_string();
        let is_request = parts[1] == "REQ";
        let is_tcp = parts[2] == "TCP";
        let unit = parts[3].parse::<u8>().unwrap_or(1);
        let func_code = u8::from_str_radix(parts[4].trim_start_matches("0x"), 16).unwrap_or(0);
        let func_name = parts[5].to_string();
        let addr = u16::from_str_radix(parts[6].trim_start_matches("0x"), 16).unwrap_or(0);
        let values: Vec<u16> = if parts.len() > 7 {
            parts[7]
                .split(';')
                .filter_map(|s| s.trim().parse::<u16>().ok())
                .collect()
        } else {
            Vec::new()
        };
        records.push(FrameRecord {
            timestamp: Instant::now(),
            human_time,
            func_code,
            func_name,
            addr,
            values,
            is_tcp,
            is_request,
            unit,
        });
    }
    Ok(records)
}

/// Load a CSV log into MonitorStats for re-analysis
pub fn load_csv_into_monitor(path: &std::path::Path) -> Result<MonitorStats> {
    let records = parse_csv_log(path)?;
    let mut stats = MonitorStats::default();
    for rec in &records {
        *stats.func_count.entry(rec.func_code).or_insert(0) += 1;
        *stats.addr_count.entry(rec.addr).or_insert(0) += 1;
        stats.total_frames += 1;
    }
    stats.history = records;
    Ok(stats)
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
        direction,
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
            let delta: u16 = rand::random_range(1..=50);
            if rand::random_bool(0.5) {
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

            if pattern == RegChangePattern::Random && rand::random_bool(0.67) {
                continue; // 67% 概率跳过（保持与原来 1~3 个变化大致相当）
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

            if pattern == RegChangePattern::Random && rand::random_bool(0.67) {
                continue;
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
        "tm" | "tcp-monitor" => Ok(MainMode::TcpMonitor),
        "rs" | "rtu-server" => Ok(MainMode::RTUServer),
        "rc" | "rtu-client" => Ok(MainMode::RTUClient),
        "rm" | "rtu-monitor" => Ok(MainMode::RtuMonitor),
        _ => Err(anyhow!(t!("main.invalid_main_mode", mode = s))),
    }
}

/// Parse combination format string to RegDataFormat
fn parse_combination_format(s: &str) -> RegDataFormat {
    let s = s.trim().to_lowercase();
    // Try exact match first (long form)
    let (data_type, width_str): (RegDataType, String) = match s.as_str() {
        "i32" | "int32" => (RegDataType::Int, "32".to_string()),
        "u32" | "uint32" => (RegDataType::Uint, "32".to_string()),
        "u64" | "uint64" => (RegDataType::Uint, "64".to_string()),
        "i64" | "int64" => (RegDataType::Int, "64".to_string()),
        "u128" | "uint128" => (RegDataType::Uint, "128".to_string()),
        "i128" | "int128" => (RegDataType::Int, "128".to_string()),
        "f16" | "half" => (RegDataType::Float, "16".to_string()),
        "f32" | "float" => (RegDataType::Float, "32".to_string()),
        "f64" | "double" => (RegDataType::Float, "64".to_string()),
        "hex" => {
            return RegDataFormat {
                data_type: RegDataType::Hex,
                width: RegDataWidth::Bits16,
            }
        }
        "bin" | "binary" => {
            return RegDataFormat {
                data_type: RegDataType::Binary,
                width: RegDataWidth::Bits16,
            }
        }
        "ascii" => {
            return RegDataFormat {
                data_type: RegDataType::Ascii,
                width: RegDataWidth::Bits16,
            }
        }
        // Parse as type+number format (e.g., "u16", "i32")
        _ => {
            let chars: Vec<char> = s.chars().collect();
            if chars.len() < 3 {
                return RegDataFormat::default();
            }
            let type_char = chars[0];
            let num_str: String = chars[1..].iter().collect();
            let dt = match type_char {
                'u' => RegDataType::Uint,
                'i' => RegDataType::Int,
                'f' => RegDataType::Float,
                _ => return RegDataFormat::default(),
            };
            (dt, num_str)
        }
    };
    let width = match width_str.as_str() {
        "16" => RegDataWidth::Bits16,
        "32" => RegDataWidth::Bits32,
        "64" => RegDataWidth::Bits64,
        "128" => RegDataWidth::Bits128,
        _ => RegDataWidth::Bits16,
    };
    RegDataFormat { data_type, width }
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

fn format_u16(v: u16, fmt: RegDataFormat) -> String {
    match fmt.data_type {
        RegDataType::Hex => format!("0x{v:04X}"),
        RegDataType::Binary => format!("0b{v:016b}"),
        _ => format!("{v}"),
    }
}

fn parse_u16_str(s: &str, fmt: RegDataFormat) -> Result<u16> {
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
    match fmt.data_type {
        RegDataType::Hex => u16::from_str_radix(t, 16).context(t!("main.parse_hex")),
        RegDataType::Binary => u16::from_str_radix(t, 2).context(t!("main.parse_bin")),
        _ => t.parse::<u16>().context(t!("main.parse_dec")),
    }
}

/// 将 f32 转换为 IEEE 754 半精度 (binary16) 的 u16 位表示
fn f32_to_f16_bits(f: f32) -> u16 {
    let bits = f.to_bits();
    let sign = ((bits >> 16) & 0x8000) as u16;
    let exp = ((bits >> 23) & 0xff) as i32;
    let mant = bits & 0x7fffff;

    if exp == 0xff {
        // Inf or NaN
        if mant == 0 {
            // Inf
            return sign | 0x7c00;
        } else {
            // NaN — preserve mantissa bits as much as possible
            return sign | 0x7c00 | ((mant >> 13) as u16);
        }
    }

    if exp == 0 {
        // Subnormal or zero
        return sign;
    }

    let unbiased_exp = exp - 127;
    // Clamp to f16 range
    if unbiased_exp > 15 {
        // overflow to inf
        return sign | 0x7c00;
    }
    if unbiased_exp < -14 {
        // underflow to zero
        return sign;
    }

    let f16_exp = (unbiased_exp + 15) as u16;
    let f16_mant = (mant >> 13) as u16;
    sign | (f16_exp << 10) | f16_mant
}

/// 将字符串值按指定格式解析，返回拆分后的 u16 寄存器值数组。
/// 支持浮点数输入（如 "3.14"）、hex ("0xFF")、binary ("0b1010") 和十进制整数。
/// swap_bytes / swap_words 的逆向操作：拆分完成后，若 swap_words 则反转字序，
/// 若 swap_bytes 则对每个字做字节交换，以匹配 format_register_value 使用的正向变换（显示→原始）。
pub(crate) fn parse_register_value(
    s: &str,
    fmt: RegDataFormat,
    swap_bytes: bool,
    swap_words: bool,
) -> Result<Vec<u16>> {
    let t = s.trim();
    if t.is_empty() {
        return Err(anyhow!(t!("main.parse_empty")));
    }

    // 辅助：应用 swap 逆操作
    let apply_swap = |words: &mut Vec<u16>| {
        if swap_words && words.len() > 1 {
            words.reverse();
        }
        if swap_bytes {
            for w in words {
                *w = w.swap_bytes();
            }
        }
    };

    let prefix_hex = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X"));
    let prefix_bin = t.strip_prefix("0b").or_else(|| t.strip_prefix("0B"));

    match (fmt.data_type, fmt.width) {
        // Single-register types
        (RegDataType::Uint, RegDataWidth::Bits16)
        | (RegDataType::Int, RegDataWidth::Bits16)
        | (RegDataType::Ascii, _) => {
            let v = parse_u16_str(t, fmt)?;
            let mut words = vec![v];
            apply_swap(&mut words);
            Ok(words)
        }
        // Hex/Binary always single-register display
        (RegDataType::Hex, _) | (RegDataType::Binary, _) => {
            let v = parse_u16_str(t, fmt)?;
            let mut words = vec![v];
            apply_swap(&mut words);
            Ok(words)
        }
        // 32-bit unsigned/signed
        (RegDataType::Uint | RegDataType::Int, RegDataWidth::Bits32) => {
            let v: u32 = if let Some(rest) = prefix_hex {
                u32::from_str_radix(rest, 16).context(t!("main.parse_hex_prefix"))?
            } else if let Some(rest) = prefix_bin {
                u32::from_str_radix(rest, 2).context(t!("main.parse_bin_prefix"))?
            } else if fmt.data_type == RegDataType::Int {
                // Signed: parse as i32, reinterpret as u32
                let signed: i32 = t.parse().context(t!("main.parse_dec"))?;
                signed as u32
            } else {
                t.parse::<u32>().context(t!("main.parse_dec"))?
            };
            let mut words = vec![(v >> 16) as u16, v as u16];
            apply_swap(&mut words);
            Ok(words)
        }
        // 64-bit unsigned/signed
        (RegDataType::Uint | RegDataType::Int, RegDataWidth::Bits64) => {
            let v: u64 = if let Some(rest) = prefix_hex {
                u64::from_str_radix(rest, 16).context(t!("main.parse_hex_prefix"))?
            } else if let Some(rest) = prefix_bin {
                u64::from_str_radix(rest, 2).context(t!("main.parse_bin_prefix"))?
            } else if fmt.data_type == RegDataType::Int {
                let signed: i64 = t.parse().context(t!("main.parse_dec"))?;
                signed as u64
            } else {
                t.parse::<u64>().context(t!("main.parse_dec"))?
            };
            let mut words = vec![
                (v >> 48) as u16,
                (v >> 32) as u16,
                (v >> 16) as u16,
                v as u16,
            ];
            apply_swap(&mut words);
            Ok(words)
        }
        // 128-bit unsigned/signed
        (RegDataType::Uint | RegDataType::Int, RegDataWidth::Bits128) => {
            let v: u128 = if let Some(rest) = prefix_hex {
                u128::from_str_radix(rest, 16).context(t!("main.parse_hex_prefix"))?
            } else if let Some(rest) = prefix_bin {
                u128::from_str_radix(rest, 2).context(t!("main.parse_bin_prefix"))?
            } else if fmt.data_type == RegDataType::Int {
                let signed: i128 = t.parse().context(t!("main.parse_dec"))?;
                signed as u128
            } else {
                t.parse::<u128>().context(t!("main.parse_dec"))?
            };
            let mut words = vec![
                (v >> 112) as u16,
                (v >> 96) as u16,
                (v >> 80) as u16,
                (v >> 64) as u16,
                (v >> 48) as u16,
                (v >> 32) as u16,
                (v >> 16) as u16,
                v as u16,
            ];
            apply_swap(&mut words);
            Ok(words)
        }
        // Float 16-bit (half precision)
        (RegDataType::Float, RegDataWidth::Bits16) => {
            let f: f32 = t.parse().context(t!("main.parse_dec"))?;
            let mut words = vec![f32_to_f16_bits(f)];
            apply_swap(&mut words);
            Ok(words)
        }
        // Float 32-bit
        (RegDataType::Float, RegDataWidth::Bits32) => {
            let f: f32 = t.parse().context(t!("main.parse_dec"))?;
            let bits = f.to_bits();
            let mut words = vec![(bits >> 16) as u16, bits as u16];
            apply_swap(&mut words);
            Ok(words)
        }
        // Float 64-bit
        (RegDataType::Float, RegDataWidth::Bits64) => {
            let f: f64 = t.parse().context(t!("main.parse_dec"))?;
            let bits = f.to_bits();
            let mut words = vec![
                (bits >> 48) as u16,
                (bits >> 32) as u16,
                (bits >> 16) as u16,
                bits as u16,
            ];
            apply_swap(&mut words);
            Ok(words)
        }
        // Float 128-bit — f128 not in std, parse as f64 (lossy) or u128
        (RegDataType::Float, RegDataWidth::Bits128) => {
            let v: u128 = if let Some(rest) = prefix_hex {
                u128::from_str_radix(rest, 16).context(t!("main.parse_hex_prefix"))?
            } else if let Some(rest) = prefix_bin {
                u128::from_str_radix(rest, 2).context(t!("main.parse_bin_prefix"))?
            } else {
                // Try f64 first, convert to bits
                let f: f64 = t.parse().context(t!("main.parse_dec"))?;
                f.to_bits() as u128
            };
            let mut words = vec![
                (v >> 112) as u16,
                (v >> 96) as u16,
                (v >> 80) as u16,
                (v >> 64) as u16,
                (v >> 48) as u16,
                (v >> 32) as u16,
                (v >> 16) as u16,
                v as u16,
            ];
            apply_swap(&mut words);
            Ok(words)
        }
    }
}

/// 将 u32 值按指定格式格式化
fn format_u32(v: u32, fmt: RegDataFormat) -> String {
    match fmt.data_type {
        RegDataType::Hex => format!("0x{v:08X}"),
        RegDataType::Binary => format!("0b{v:032b}"),
        _ => format!("{v}"),
    }
}

/// 将 u64 值按指定格式格式化
fn format_u64(v: u64, fmt: RegDataFormat) -> String {
    match fmt.data_type {
        RegDataType::Hex => format!("0x{v:016X}"),
        RegDataType::Binary => format!("0b{v:064b}"),
        _ => format!("{v}"),
    }
}

/// 将 i16 值按指定格式格式化
fn format_i16(v: i16, fmt: RegDataFormat) -> String {
    match fmt.data_type {
        RegDataType::Hex => format!("0x{v:04X}"),
        RegDataType::Binary => format!("0b{v:016b}"),
        _ => format!("{v}"),
    }
}

/// 将 i32 值按指定格式格式化
fn format_i32(v: i32, fmt: RegDataFormat) -> String {
    match fmt.data_type {
        RegDataType::Hex => format!("0x{v:08X}"),
        RegDataType::Binary => format!("0b{v:032b}"),
        _ => format!("{v}"),
    }
}

/// 将 i64 值按指定格式格式化
fn format_i64(v: i64, fmt: RegDataFormat) -> String {
    match fmt.data_type {
        RegDataType::Hex => format!("0x{v:016X}"),
        RegDataType::Binary => format!("0b{v:064b}"),
        _ => format!("{v}"),
    }
}

/// 将 u128 值按指定格式格式化
fn format_u128(v: u128, fmt: RegDataFormat) -> String {
    match fmt.data_type {
        RegDataType::Hex => format!("0x{v:032X}"),
        RegDataType::Binary => format!("0b{v:0128b}"),
        _ => format!("{v}"),
    }
}

/// 将 i128 值按指定格式格式化
fn format_i128(v: i128, fmt: RegDataFormat) -> String {
    match fmt.data_type {
        RegDataType::Hex => format!("0x{v:032X}"),
        RegDataType::Binary => format!("0b{v:0128b}"),
        _ => format!("{v}"),
    }
}

/// 半精度浮点 (f16) 转字符串
fn format_f16(h: u16) -> String {
    let sign = if (h & 0x8000) != 0 { -1.0 } else { 1.0 };
    let exp = (h >> 10) & 0x1f;
    let mant = (h & 0x3ff) as f32;
    let v = match exp {
        0 => {
            if mant == 0.0 {
                0.0
            } else {
                sign * mant / 1024.0 * 2.0_f32.powi(-14)
            }
        }
        31 => {
            if mant == 0.0 {
                if sign > 0.0 {
                    f32::INFINITY
                } else {
                    f32::NEG_INFINITY
                }
            } else {
                f32::NAN
            }
        }
        _ => {
            let exp_val = (exp as i32) - 15;
            sign * (1.0 + mant / 1024.0) * 2.0_f32.powi(exp_val)
        }
    };
    format!("{:.6}", v)
}

/// 根据寄存器数据解释格式格式化一个地址的值
pub(crate) fn format_register_value(
    regs: &[u16],
    addr: usize,
    format: RegDataFormat,
    swap_bytes: bool,
    swap_words: bool,
) -> String {
    let needed = format.regs_needed();
    if addr + needed > regs.len() {
        return format!("-- (need {})", needed);
    }

    let mut words: Vec<u16> = regs[addr..addr + needed].to_vec();

    // 字节交换
    if swap_bytes {
        for w in &mut words {
            *w = w.swap_bytes();
        }
    }
    // 字序交换（多寄存器格式）
    if swap_words && words.len() > 1 {
        words.reverse();
    }

    match (format.data_type, format.width) {
        (RegDataType::Uint, RegDataWidth::Bits16)
        | (RegDataType::Hex, _)
        | (RegDataType::Binary, _)
        | (RegDataType::Ascii, _) => format_u16(words[0], format),
        (RegDataType::Int, RegDataWidth::Bits16) => format_i16(words[0] as i16, format),
        (RegDataType::Uint | RegDataType::Int, RegDataWidth::Bits32) => {
            let v = (words[0] as u32) << 16 | words[1] as u32;
            if format.data_type == RegDataType::Int {
                format_i32(v as i32, format)
            } else {
                format_u32(v, format)
            }
        }
        (RegDataType::Uint | RegDataType::Int, RegDataWidth::Bits64) => {
            let v = (words[0] as u64) << 48
                | (words[1] as u64) << 32
                | (words[2] as u64) << 16
                | words[3] as u64;
            if format.data_type == RegDataType::Int {
                format_i64(v as i64, format)
            } else {
                format_u64(v, format)
            }
        }
        (RegDataType::Uint | RegDataType::Int, RegDataWidth::Bits128) => {
            let v = (words[0] as u128) << 112
                | (words[1] as u128) << 96
                | (words[2] as u128) << 80
                | (words[3] as u128) << 64
                | (words[4] as u128) << 48
                | (words[5] as u128) << 32
                | (words[6] as u128) << 16
                | words[7] as u128;
            if format.data_type == RegDataType::Int {
                format_i128(v as i128, format)
            } else {
                format_u128(v, format)
            }
        }
        (RegDataType::Float, RegDataWidth::Bits16) => format_f16(words[0]),
        (RegDataType::Float, RegDataWidth::Bits32) => {
            let bits = ((words[0] as u32) << 16) | words[1] as u32;
            let v = f32::from_bits(bits);
            format!("{:.6}", v)
        }
        (RegDataType::Float, RegDataWidth::Bits64) => {
            let bits = (words[0] as u64) << 48
                | (words[1] as u64) << 32
                | (words[2] as u64) << 16
                | words[3] as u64;
            let v = f64::from_bits(bits);
            format!("{:.10}", v)
        }
        (RegDataType::Float, RegDataWidth::Bits128) => {
            let bits = (words[0] as u128) << 112
                | (words[1] as u128) << 96
                | (words[2] as u128) << 80
                | (words[3] as u128) << 64
                | (words[4] as u128) << 48
                | (words[5] as u128) << 32
                | (words[6] as u128) << 16
                | words[7] as u128;
            format_u128(bits, format)
        }
    }
}

/// 导出当前所有寄存器值为 JSON
pub(crate) fn export_registers_to_json(
    reg_format: RegDataFormat,
    state: &AppState,
) -> Result<(String, String)> {
    use serde::Serialize;

    #[derive(Serialize)]
    struct ExportData {
        export_time: String,
        reg_format: String,
        holding: Vec<ExportReg>,
        coils: Vec<bool>,
        discrete: Vec<bool>,
        input_registers: Vec<ExportReg>,
    }

    #[derive(Serialize)]
    struct ExportReg {
        addr: usize,
        raw: u16,
        label: String,
        value: String,
    }

    fn make_regs(
        regs: &[u16],
        labels: &[String],
        reg_format: RegDataFormat,
        swap_bytes_map: &HashMap<usize, bool>,
        swap_words_map: &HashMap<usize, bool>,
    ) -> Vec<ExportReg> {
        regs.iter()
            .enumerate()
            .map(|(i, &raw)| {
                let sb = swap_bytes_map.get(&i).copied().unwrap_or(false);
                let sw = swap_words_map.get(&i).copied().unwrap_or(false);
                let value = format_register_value(regs, i, reg_format, sb, sw);
                let label = labels.get(i).cloned().unwrap_or_default();
                ExportReg {
                    addr: i,
                    raw,
                    label,
                    value,
                }
            })
            .collect()
    }

    let now = chrono_now();

    let data = ExportData {
        export_time: now.clone(),
        reg_format: reg_format.short_label().to_string(),
        holding: make_regs(
            &state.holding,
            &state.holding_label,
            reg_format,
            &state.holding_swap_bytes,
            &state.holding_swap_words,
        ),
        coils: state.coils.clone(),
        discrete: state.discrete.clone(),
        input_registers: make_regs(
            &state.input_registers,
            &[],
            reg_format,
            &state.input_swap_bytes,
            &state.input_swap_words,
        ),
    };

    let json =
        serde_json::to_string_pretty(&data).context("Failed to serialize registers to JSON")?;
    let filename = format!(
        "modbus_export_{}.json",
        now.replace(' ', "_").replace(':', "-")
    );
    Ok((filename, json))
}

fn chrono_now() -> String {
    use std::time::SystemTime;
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    // Simple local time formatting
    let days = secs / 86400;
    let time_secs = secs % 86400;
    let hours = time_secs / 3600;
    let minutes = (time_secs % 3600) / 60;
    let seconds = time_secs % 60;
    format!(
        "{:04}-{:02}-{:02}_{:02}-{:02}-{:02}",
        1970 + (days / 365) as u32,
        1,
        1,
        hours,
        minutes,
        seconds
    )
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
        MainMode::TcpMonitor => "tcp-monitor".into(),
        MainMode::RtuMonitor => "rtu-monitor".into(),
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
    let (main_mode, args, profile_name) = if let Some(profile_name) = &cli_args.profile {
        let config_str = std::fs::read_to_string(&cli_args.config)
            .with_context(|| t!("main.read_config_fail", path = &cli_args.config))?;
        let configs = toml::from_str::<HashMap<String, Args>>(&config_str)
            .with_context(|| t!("main.parse_config_fail", path = &cli_args.config))?;
        if let Some(profile_args) = configs.get(profile_name) {
            let mut args = profile_args.clone();
            let main_mode = parse_mainmode(&args.main_mode)?;
            args.config = config_path_base;
            (main_mode, args, cli_args.profile.clone())
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
        let pname = sel.profile_name.clone();
        // 用户选择的配置解析
        let (main_mode, args) = resolve_selection(&cli_args.config, &sel)?;
        (main_mode, args, pname)
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
    // Parse reg_combinations from profile into HashMap<usize, RegDataFormat>
    let mut holding_combinations: HashMap<usize, RegDataFormat> = HashMap::new();
    let mut input_combinations: HashMap<usize, RegDataFormat> = HashMap::new();
    for (k, v) in &args.reg_combinations {
        // Keys prefixed with "i:" are input register combinations, otherwise holding
        if let Some(addr_str) = k.strip_prefix("i:") {
            if let Ok(idx) = addr_str.parse::<usize>() {
                let fmt = parse_combination_format(v);
                if fmt != RegDataFormat::default() {
                    input_combinations.insert(idx, fmt);
                }
            }
        } else if let Ok(idx) = k.parse::<usize>() {
            let fmt = parse_combination_format(v);
            if fmt != RegDataFormat::default() {
                holding_combinations.insert(idx, fmt);
            }
        }
    }
    // Parse swap_bytes_reg / swap_words_reg from profile
    let mut holding_swap_bytes: HashMap<usize, bool> = HashMap::new();
    let mut holding_swap_words: HashMap<usize, bool> = HashMap::new();
    let mut input_swap_bytes: HashMap<usize, bool> = HashMap::new();
    let mut input_swap_words: HashMap<usize, bool> = HashMap::new();
    for (k, v) in &args.swap_bytes_reg {
        if v == "true" {
            if let Some(addr_str) = k.strip_prefix("i:") {
                if let Ok(idx) = addr_str.parse::<usize>() {
                    input_swap_bytes.insert(idx, true);
                }
            } else if let Ok(idx) = k.parse::<usize>() {
                holding_swap_bytes.insert(idx, true);
            }
        }
    }
    for (k, v) in &args.swap_words_reg {
        if v == "true" {
            if let Some(addr_str) = k.strip_prefix("i:") {
                if let Ok(idx) = addr_str.parse::<usize>() {
                    input_swap_words.insert(idx, true);
                }
            } else if let Ok(idx) = k.parse::<usize>() {
                holding_swap_words.insert(idx, true);
            }
        }
    }
    let state = Arc::new(RwLock::new(AppState {
        holding: vec![0u16; binding_count],
        holding_label,
        coils: vec![false; args.coil_count],
        discrete: vec![false; args.discrete_count],
        input_registers: vec![0u16; args.input_count],
        last_frame: None,
        is_tcp: matches!(
            main_mode,
            MainMode::TcpServer | MainMode::TcpClient | MainMode::TcpMonitor
        ),
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
        reg_format: RegDataFormat::default(),
        holding_swap_bytes,
        holding_swap_words,
        input_swap_bytes,
        input_swap_words,
        holding_combinations,
        input_combinations,
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
            MainMode::TcpMonitor | MainMode::RtuMonitor => {
                // 监听模式不需要 modbus 后端，仅保持运行
                futures::future::pending::<Result<()>>().await
            }
        };

        if let Err(e) = &r {
            *inner_status_bg.write().await = Some(format!("{e:#}"));
        }

        r
    });
    // 加载配置列表（供监听模式使用）
    let config_path = cli_args.config.clone();
    let profiles = load_profile_list(&config_path);
    let ui_res = if main_mode == MainMode::TcpClient
        || main_mode == MainMode::RTUClient
        || main_mode == MainMode::TcpMonitor
        || main_mode == MainMode::RtuMonitor
    {
        run_ui(
            Arc::clone(&state),
            client_tx,
            args,
            server_status,
            config_path,
            profiles,
            profile_name,
        )
        .await
    } else {
        run_ui(
            Arc::clone(&state),
            server_tx,
            args,
            server_status,
            config_path,
            profiles,
            profile_name,
        )
        .await
    };

    inner_task.abort();
    let _ = inner_task.await;

    ui_res
}
