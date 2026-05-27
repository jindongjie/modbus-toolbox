use tokio::time::Duration;

use ratatui::layout::{Constraint, Layout, Rect};

// crate
use crate::modbus::frame_bytes_from_info;
use crate::{
    AppState, Args, FrameInfo, FrameRecord, MainMode, RegDataFormat, RegDataType, RegDataWidth,
};

pub mod client_server;
pub mod menu;
pub mod monitor;
pub mod tests_raw;

/// 从字符串解析寄存器数据格式
pub fn parse_reg_format(s: &str) -> RegDataFormat {
    match s.trim().to_lowercase().as_str() {
        "i16" => RegDataFormat {
            data_type: RegDataType::Int,
            width: RegDataWidth::Bits16,
        },
        "u32" | "uint32" => RegDataFormat {
            data_type: RegDataType::Uint,
            width: RegDataWidth::Bits32,
        },
        "i32" | "int32" => RegDataFormat {
            data_type: RegDataType::Int,
            width: RegDataWidth::Bits32,
        },
        "u64" | "uint64" => RegDataFormat {
            data_type: RegDataType::Uint,
            width: RegDataWidth::Bits64,
        },
        "i64" | "int64" => RegDataFormat {
            data_type: RegDataType::Int,
            width: RegDataWidth::Bits64,
        },
        "u128" | "uint128" => RegDataFormat {
            data_type: RegDataType::Uint,
            width: RegDataWidth::Bits128,
        },
        "i128" | "int128" => RegDataFormat {
            data_type: RegDataType::Int,
            width: RegDataWidth::Bits128,
        },
        "f16" | "half" => RegDataFormat {
            data_type: RegDataType::Float,
            width: RegDataWidth::Bits16,
        },
        "f32" | "float" => RegDataFormat {
            data_type: RegDataType::Float,
            width: RegDataWidth::Bits32,
        },
        "f64" | "double" => RegDataFormat {
            data_type: RegDataType::Float,
            width: RegDataWidth::Bits64,
        },
        "hex" => RegDataFormat {
            data_type: RegDataType::Hex,
            width: RegDataWidth::Bits16,
        },
        "bin" | "binary" => RegDataFormat {
            data_type: RegDataType::Binary,
            width: RegDataWidth::Bits16,
        },
        "ascii" => RegDataFormat {
            data_type: RegDataType::Ascii,
            width: RegDataWidth::Bits16,
        },
        _ => RegDataFormat::default(),
    }
}

const UI_TIMEOUT: Duration = Duration::from_secs(5);

/// 菜单屏幕状态
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum MenuScreen {
    Main,        // 主菜单
    ProfilePick, // 选择配置（Server/Client 子菜单）
    ProfileSet,  // 配置管理设置
    ProfileEdit, // 编辑配置字段
    NamePrompt,  // 输入新增/克隆的配置名
}

/// 菜单选择结果，返回给 main.rs
#[derive(Clone, Debug)]
pub struct MenuSelection {
    pub main_mode: MainMode,
    pub profile_name: Option<String>,
    pub quit: bool,
}

pub struct Ui {
    pub selected: usize,
    pub scroll: usize,
    pub edit_mode: bool,
    pub edit_is_label: bool,
    pub edit_is_profile: bool,
    pub edit_buf: String,
    pub status_msg: Option<String>,
    pub show_byte_panel: bool,

    // --- 静默监听 ---
    pub show_monitor: bool,
    /// 历史记录滚动偏移
    pub monitor_scroll: usize,
    /// true=焦点在历史面板, false=焦点在统计面板
    pub monitor_focus_history: bool,

    // --- 菜单相关字段 ---
    /// 当前菜单屏幕：Main / ProfilePick / ProfileSet
    pub menu_screen: MenuScreen,
    /// ProfilePick/ProfileSet 以及主菜单中的选中项索引
    pub menu_list_idx: usize,
    /// 加载到的所有配置名
    pub profiles: Vec<String>,
    /// 当前选中的配置
    pub selected_profile: Option<String>,
    /// 待选模式（从主菜单进入子菜单时暂存）
    pub pending_mode: Option<MainMode>,
    /// 当前默认配置名
    pub default_profile: Option<String>,
    /// 主菜单渲染用的 Logo ASCII ART 行（正确/最终内容）
    pub logo_target: Vec<String>,
    /// Logo 动画当前显示的字符（随机→正确渐变）
    pub logo_current: Vec<String>,
    /// Logo 动画帧计数器（递增到 LOGO_ANIM_FRAMES 后停止，全部显示正确字符）
    pub logo_frame: u32,

    // --- 配置编辑相关字段 ---
    /// 正在编辑的配置名
    pub edit_profile_name: Option<String>,
    /// 正在编辑的配置参数副本
    pub edit_args: Option<Args>,
    /// 编辑字段缓冲
    pub field_edit_buf: String,
    /// 是否正在编辑字段值（true=编辑模式，false=导航模式）
    pub field_edit_mode: bool,
    /// 可用串口列表（启动时预检测并缓存）
    pub serial_ports: Vec<String>,
    /// 串口端口索引（用于设备字段编辑时循环选择）
    pub serial_port_idx: usize,

    // --- 新增/克隆配置相关字段 ---
    /// 新增/克隆时的名称输入缓冲
    pub name_prompt_buf: String,
    /// true=克隆选中配置, false=新建空配置
    pub name_prompt_is_clone: bool,
    /// 克隆时暂存的原配置 Args
    pub name_prompt_clone_args: Option<Args>,

    // --- 监听模式配置选择 ---
    /// 监听模式下是否正在选择配置
    pub monitor_picking: bool,
    /// 监听模式下已选中的配置名
    pub monitor_selected_profile: Option<String>,
    /// 筛选后的配置名列表（按 pending_mode 过滤）
    pub pick_names: Vec<String>,
    /// 筛选后的配置简介列表（与 pick_names 一一对应）
    pub pick_briefs: Vec<String>,
    /// 配置文件路径
    pub config_path: String,

    // --- CSV 日志记录 ---
    /// 是否正在记录监听数据到 CSV
    pub monitor_logging: bool,
    /// 当前 CSV 日志文件路径
    pub monitor_log_path: Option<std::path::PathBuf>,
    /// CSV 文件选择模式
    pub csv_picking: bool,
    /// CSV 文件列表
    pub csv_files: Vec<std::path::PathBuf>,
    /// CSV 文件选择索引
    pub csv_pick_idx: usize,
    /// 是否正在回放 CSV 文件（用于区分 live 和 replay 模式）
    pub csv_replay_active: bool,
    /// 上次已记录到 CSV 的帧数（用于增量写入）
    pub last_logged_frames: usize,

    // --- 寄存器视图选择 ---
    /// 当前显示的寄存器类型：0=保持寄存器, 1=线圈, 2=离散输入, 3=输入寄存器
    pub reg_view: usize,

    // --- 协议分析对话框 ---
    /// 是否显示协议分析弹窗
    pub show_analysis_dialog: bool,
    /// 被分析的历史记录索引
    pub analysis_idx: usize,
    /// 从设备扫描结果对话框
    pub show_scan_dialog: bool,
    /// 寄存器变化模式配置对话框：是否打开
    pub pattern_dialog_open: bool,
    /// 正在配置的寄存器地址
    pub pattern_dialog_addr: usize,
    /// 寄存器类型 (REG_VIEW_HOLDING=0, REG_VIEW_INPUT=3)
    pub pattern_dialog_reg_type: usize,
    /// 当前选中的模式索引 (0=Random, 1=UpDown, 2=Sine, 3=Square, 4=Triangle)
    pub pattern_dialog_sel: usize,
    /// 是否正在编辑频率
    pub pattern_dialog_editing_freq: bool,
    /// 频率编辑缓存
    pub pattern_dialog_freq_buf: String,
    /// 临时存储的频率值
    pub pattern_dialog_freq: f64,
    /// 是否显示值变化历史条形图
    pub show_change_bar: bool,

    // --- 跳转寄存器地址 ---
    /// 是否正在输入目标寄存器地址
    pub goto_mode: bool,
    /// 地址输入缓存
    pub goto_buf: String,
    /// 搜索模式
    pub search_mode: bool,
    /// 搜索缓冲
    pub search_buf: String,
    /// 当前寄存器数据解释格式（UI 本地副本，与 AppState 同步）
    pub reg_format: RegDataFormat,

    // --- 配置信息弹窗 ---
    /// 是否显示配置信息弹窗
    pub show_profile_info: bool,
    /// 当前配置的完整参数（用于弹窗显示）
    pub args: Args,
}

/// 伪随机数生成（线性同余），用于 logo 动画，不依赖 rand crate
fn lcg(seed: u64) -> u64 {
    seed.wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407)
}

/// 根据 (row, col, frame) 生成一个伪随机的可打印 ASCII 字符
fn logo_random_char(row: usize, col: usize, frame: u8) -> char {
    let seed = (row as u64)
        .wrapping_mul(10007)
        .wrapping_add((col as u64).wrapping_mul(50021))
        .wrapping_add(frame as u64);
    let h = lcg(seed);
    // 33..=126 的可打印 ASCII 范围
    let idx = ((h >> 32) as u32) % 94;
    char::from_u32(idx + 33).unwrap_or('?')
}

/// 创建一个全部为随机字符的 logo 显示缓冲区
fn logo_random_buf(target: &[String]) -> Vec<String> {
    target
        .iter()
        .enumerate()
        .map(|(row, line)| {
            line.chars()
                .enumerate()
                .map(|(col, _ch)| {
                    // 空白字符保持原样，非空白替换为随机字符
                    if _ch == ' ' {
                        ' '
                    } else {
                        logo_random_char(row, col, 0)
                    }
                })
                .collect()
        })
        .collect()
}

/// 寄存器视图类型常量
const REG_VIEW_HOLDING: usize = 0;
const REG_VIEW_COILS: usize = 1;
const REG_VIEW_DISCRETE: usize = 2;
const REG_VIEW_INPUT: usize = 3;

impl Ui {
    fn new(reg_format: RegDataFormat, profiles: Vec<String>) -> Self {
        let logo_raw = include_str!("../logo.txt");
        let mut logo_target: Vec<String> = logo_raw.lines().map(|l| l.to_string()).collect();
        logo_target.push(format!("        v{}", env!("CARGO_PKG_VERSION")));
        let logo_current = logo_random_buf(&logo_target);
        let default = profiles.first().cloned();
        // 检测可用串口列表
        let serial_ports = tokio_serial::available_ports()
            .ok()
            .map(|ports| ports.into_iter().map(|p| p.port_name).collect())
            .unwrap_or_default();
        Self {
            selected: 0,
            scroll: 0,
            edit_mode: false,
            edit_is_label: false,
            edit_is_profile: false,
            edit_buf: String::new(),
            status_msg: None,
            show_byte_panel: false,
            show_monitor: false,
            monitor_scroll: 0,
            monitor_focus_history: true,

            menu_screen: MenuScreen::Main,
            menu_list_idx: 0,
            profiles,
            selected_profile: default.clone(),
            pending_mode: None,
            default_profile: default,
            logo_target,
            logo_current,
            logo_frame: 0,
            serial_ports,

            // 配置编辑
            edit_profile_name: None,
            edit_args: None,
            field_edit_buf: String::new(),
            field_edit_mode: false,
            serial_port_idx: 0,

            // 新增/克隆
            name_prompt_buf: String::new(),
            name_prompt_is_clone: false,
            name_prompt_clone_args: None,

            // 监听模式配置选择
            monitor_picking: true, // 启动时直接进入选择模式
            monitor_selected_profile: None,
            pick_names: Vec::new(),
            pick_briefs: Vec::new(),
            config_path: String::new(),

            // CSV 日志
            monitor_logging: false,
            monitor_log_path: None,
            csv_picking: false,
            csv_files: Vec::new(),
            csv_pick_idx: 0,
            csv_replay_active: false,
            last_logged_frames: 0,

            // 寄存器视图
            reg_view: REG_VIEW_HOLDING,

            show_analysis_dialog: false,
            analysis_idx: 0,
            show_scan_dialog: false,
            pattern_dialog_open: false,
            pattern_dialog_addr: 0,
            pattern_dialog_reg_type: 0,
            pattern_dialog_sel: 0,
            pattern_dialog_editing_freq: false,
            pattern_dialog_freq_buf: String::new(),
            pattern_dialog_freq: 1.0,
            show_change_bar: false,
            goto_mode: false,
            goto_buf: String::new(),
            search_mode: false,
            search_buf: String::new(),
            reg_format,
            show_profile_info: false,
            args: crate::Args::default(),
        }
    }
}

/// 获取当前寄存器视图的数据和可选标签
pub fn reg_view_data(s: &AppState, reg_view: usize) -> (&[u16], Option<&[String]>) {
    match reg_view {
        REG_VIEW_HOLDING => (&s.holding, Some(&s.holding_label)),
        REG_VIEW_COILS => {
            // 对 bool 类型返回引用标记
            let mapped: Vec<u16> = s.coils.iter().map(|&b| if b { 1 } else { 0 }).collect();
            (Box::leak(mapped.into_boxed_slice()), None)
        }
        REG_VIEW_DISCRETE => {
            let mapped: Vec<u16> = s.discrete.iter().map(|&b| if b { 1 } else { 0 }).collect();
            (Box::leak(mapped.into_boxed_slice()), None)
        }
        REG_VIEW_INPUT => (&s.input_registers, None),
        _ => (&s.holding, Some(&s.holding_label)),
    }
}

/// 获取当前寄存器视图的长度
pub fn reg_view_len(s: &AppState, reg_view: usize) -> usize {
    match reg_view {
        REG_VIEW_HOLDING => s.holding.len(),
        REG_VIEW_COILS => s.coils.len(),
        REG_VIEW_DISCRETE => s.discrete.len(),
        REG_VIEW_INPUT => s.input_registers.len(),
        _ => s.holding.len(),
    }
}

/// 检查 register 是否匹配搜索词（按地址或标签）
pub fn search_match(
    idx: usize,
    search_lower: &str,
    _items: &[u16],
    labels: Option<&[String]>,
) -> bool {
    let idx_str = format!("{}", idx);
    if idx_str.contains(search_lower) {
        return true;
    }
    if let Some(lbl) = labels.and_then(|l| l.get(idx)) {
        if lbl.to_lowercase().contains(search_lower) {
            return true;
        }
    }
    false
}

#[allow(dead_code)]
pub fn edit_accepts_char(current: &str, ch: char, fmt: RegDataFormat) -> bool {
    if ch.is_ascii_whitespace() {
        return false;
    }
    if ch == 'x' || ch == 'X' || ch == 'b' || ch == 'B' {
        return current == "0" || current.eq_ignore_ascii_case("0");
    }
    if ch == '0' && current.is_empty() {
        return true;
    }
    match fmt.data_type {
        RegDataType::Hex => ch.is_ascii_hexdigit(),
        RegDataType::Binary => ch == '0' || ch == '1',
        RegDataType::Float => {
            ch.is_ascii_digit() || ch == '-' || ch == '.' || ch == 'e' || ch == 'E' || ch == '+'
        }
        _ => ch.is_ascii_digit() || ch == '-',
    }
}

pub fn set_status(ui: &mut Ui, msg: impl Into<String>) {
    ui.status_msg = Some(msg.into());
}

/// 计算自动换行后的实际行数
pub fn wrapped_lines(text: &str, max_width: usize) -> usize {
    if max_width == 0 {
        return 1;
    }
    text.lines()
        .map(|line| {
            let len = line.len();
            if len == 0 {
                1
            } else {
                len.div_ceil(max_width)
            }
        })
        .sum()
}

/// 居中矩形辅助函数
pub fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::vertical([
        Constraint::Length((r.height * (100 - percent_y)) / 200),
        Constraint::Length((r.height * percent_y) / 100),
        Constraint::Min(0),
    ])
    .split(r);
    Layout::horizontal([
        Constraint::Length((r.width * (100 - percent_x)) / 200),
        Constraint::Length((r.width * percent_x) / 100),
        Constraint::Min(0),
    ])
    .split(popup_layout[1])[1]
}

/// 格式化协议分析文本（用于弹窗）
pub fn format_protocol_analysis(rec: &FrameRecord) -> String {
    use crate::modbus::calc_crc16;

    // 将 FrameRecord 转为 FrameInfo
    let fi = FrameInfo {
        is_tcp: rec.is_tcp,
        unit: rec.unit,
        func_code: rec.func_code,
        func_name: rec.func_name.clone(),
        addr: rec.addr,
        values: rec.values.clone(),
        is_request: rec.is_request,
    };

    // 复用已有的字节面板分析
    let mut text = format_byte_panel(&fi);

    // 追加 CRC 校验结果
    if !rec.is_tcp {
        let bytes = frame_bytes_from_info(&fi);
        if bytes.len() >= 3 {
            let data_len = bytes.len() - 2;
            let calc = calc_crc16(&bytes[..data_len]);
            let stored = u16::from_le_bytes([bytes[data_len], bytes[data_len + 1]]);
            let ok = calc == stored;
            text.push_str(&format!("\n{}\n", t!("byte_panel.crc_verify_title")));
            text.push_str(&format!(
                "  {} {:04X}\n",
                t!("byte_panel.crc_calculated"),
                calc
            ));
            text.push_str(&format!(
                "  {} {:04X}\n",
                t!("byte_panel.crc_stored"),
                stored
            ));
            text.push_str(&format!(
                "  {}",
                if ok {
                    t!("byte_panel.crc_match")
                } else {
                    t!("byte_panel.crc_mismatch")
                }
            ));
        }
    } else {
        // TCP 无 CRC，显示 MBAP header 简析
        if rec.is_request {
            text.push_str(&format!("\n  {}", t!("byte_panel.tcp_req_note")));
        } else {
            text.push_str(&format!("\n  {}", t!("byte_panel.tcp_resp_note")));
        }
    }

    text
}

/// 构建字节流面板的显示文本
pub fn format_byte_panel(fi: &FrameInfo) -> String {
    let mut text = String::new();
    let mut bytes = frame_bytes_from_info(fi);
    if bytes.is_empty() {
        return t!("byte_panel.build_fail").to_string();
    }

    let header_line = if fi.is_tcp {
        t!("byte_panel.tcp_response", func = &fi.func_name)
    } else {
        t!("byte_panel.rtu_response", func = &fi.func_name)
    };
    text.push_str(&format!("{}\n", header_line));
    text.push_str("━━━━━━━━━━━━━━━━━\n");
    text.push_str(&format!("{}\n", t!("byte_panel.header")));

    if fi.is_tcp {
        // TCP 帧头
        let trans_id = u16::from_be_bytes([bytes[0], bytes[1]]);
        let proto_id = u16::from_be_bytes([bytes[2], bytes[3]]);
        let len = u16::from_be_bytes([bytes[4], bytes[5]]);
        let unit = bytes[6];
        text.push_str(&format!(
            "0-1   {:04X}  {}\n",
            trans_id,
            t!("byte_panel.transaction_id", id = trans_id)
        ));
        let proto_desc = if proto_id == 0 {
            t!("byte_panel.protocol_id_modbus")
        } else {
            t!("byte_panel.protocol_id_other")
        };
        text.push_str(&format!("2-3   {:04X}  {}\n", proto_id, proto_desc));
        text.push_str(&format!(
            "4-5   {:04X}  {}\n",
            len,
            t!("byte_panel.length", len = len)
        ));
        text.push_str(&format!(
            "6     {:02X}    {}\n",
            unit,
            t!("byte_panel.unit", unit = unit)
        ));

        // 从字节7开始是功能码
        let func = bytes[7];
        text.push_str(&format!(
            "7     {:02X}    {}\n",
            func,
            t!("byte_panel.func_code", name = &fi.func_name, code = func)
        ));

        // 截取剩余部分作为原始帧
        bytes = bytes[8..].to_vec();
    } else {
        // RTU 帧: [unit] [func] [data...] [crc]
        let unit = bytes[0];
        let func = bytes[1];
        text.push_str(&format!(
            "0     {:02X}    {}\n",
            unit,
            t!("byte_panel.unit", unit = unit)
        ));
        text.push_str(&format!(
            "1     {:02X}    {}\n",
            func,
            t!("byte_panel.func_code", name = &fi.func_name, code = func)
        ));

        // 跳过地址和功能码，保留数据+CRC
        bytes = bytes[2..].to_vec();
    }

    // 解析剩余数据
    let func_body = &bytes;

    if func_body.len() >= 2 {
        match fi.func_code {
            0x03 => {
                // 响应: [byte_count] [data...] [crc...]
                let bc = func_body[0] as usize;
                let offset_base = if fi.is_tcp { 8usize } else { 2usize };
                text.push_str(&format!(
                    "{:<5} {:02X}    {}\n",
                    if fi.is_tcp { 8 } else { 2 },
                    bc,
                    t!("byte_panel.byte_count", count = bc)
                ));
                let mut data_offset = 1;
                let mut reg_idx = 0;
                while data_offset + 1 < func_body.len().saturating_sub(2)
                    && reg_idx < fi.values.len()
                {
                    if data_offset + 1 >= func_body.len() {
                        break;
                    }
                    let h = func_body[data_offset];
                    let l = func_body[data_offset + 1];
                    let val = u16::from_be_bytes([h, l]);
                    let abs_offset = offset_base + data_offset;
                    text.push_str(&format!(
                        "{}-{} {:04X}  {}\n",
                        abs_offset,
                        abs_offset + 1,
                        val,
                        t!("byte_panel.register", idx = reg_idx, val = val)
                    ));
                    data_offset += 2;
                    reg_idx += 1;
                }
                // CRC (最后2字节)
                if func_body.len() >= 2 {
                    let crc_start = func_body.len() - 2;
                    let crc_val =
                        u16::from_le_bytes([func_body[crc_start], func_body[crc_start + 1]]);
                    let abs_crc = offset_base + crc_start;
                    text.push_str(&format!(
                        "{}-{} {:04X}  {}\n",
                        abs_crc,
                        abs_crc + 1,
                        crc_val,
                        t!("byte_panel.crc16", crc = crc_val)
                    ));
                }
            }
            0x06 => {
                // 写单寄存器响应: [addr hi] [addr lo] [val hi] [val lo] [crc...]
                let offset_base = if fi.is_tcp { 8usize } else { 2usize };
                if func_body.len() >= 4 {
                    let addr_val = u16::from_be_bytes([func_body[0], func_body[1]]);
                    let data_val = u16::from_be_bytes([func_body[2], func_body[3]]);
                    text.push_str(&format!(
                        "{}-{} {:04X}  {}\n",
                        offset_base,
                        offset_base + 1,
                        addr_val,
                        t!("byte_panel.register_addr", addr = addr_val)
                    ));
                    text.push_str(&format!(
                        "{}-{} {:04X}  {}\n",
                        offset_base + 2,
                        offset_base + 3,
                        data_val,
                        t!("byte_panel.register_val", val = data_val)
                    ));
                    if !fi.is_tcp && func_body.len() >= 6 {
                        let crc_start = func_body.len() - 2;
                        let crc_val =
                            u16::from_le_bytes([func_body[crc_start], func_body[crc_start + 1]]);
                        let abs_crc = offset_base + crc_start;
                        text.push_str(&format!(
                            "{}-{} {:04X}  {}\n",
                            abs_crc,
                            abs_crc + 1,
                            crc_val,
                            t!("byte_panel.crc16", crc = crc_val)
                        ));
                    }
                }
            }
            0x10 => {
                // 写多寄存器响应: [addr hi] [addr lo] [qty hi] [qty lo] [crc...]
                let offset_base = if fi.is_tcp { 8usize } else { 2usize };
                if func_body.len() >= 4 {
                    let addr_val = u16::from_be_bytes([func_body[0], func_body[1]]);
                    let qty = u16::from_be_bytes([func_body[2], func_body[3]]);
                    text.push_str(&format!(
                        "{}-{} {:04X}  {}\n",
                        offset_base,
                        offset_base + 1,
                        addr_val,
                        t!("byte_panel.start_addr", addr = addr_val)
                    ));
                    text.push_str(&format!(
                        "{}-{} {:04X}  {}\n",
                        offset_base + 2,
                        offset_base + 3,
                        qty,
                        t!("byte_panel.register_qty", qty = qty)
                    ));
                    if !fi.is_tcp && func_body.len() >= 6 {
                        let crc_start = func_body.len() - 2;
                        let crc_val =
                            u16::from_le_bytes([func_body[crc_start], func_body[crc_start + 1]]);
                        let abs_crc = offset_base + crc_start;
                        text.push_str(&format!(
                            "{}-{} {:04X}  {}\n",
                            abs_crc,
                            abs_crc + 1,
                            crc_val,
                            t!("byte_panel.crc16", crc = crc_val)
                        ));
                    }
                }
            }
            _ => {}
        }
    }

    // 原始帧
    text.push_str("━━━━━━━━━━━━━━━━━\n");
    let full_bytes = frame_bytes_from_info(fi);
    let hex_str: Vec<String> = full_bytes.iter().map(|b| format!("{:02X}", b)).collect();
    let lines = hex_str.chunks(8);
    text.push_str(&format!("{}\n", t!("byte_panel.raw_frame")));
    for line in lines {
        text.push_str(&format!("  {}\n", line.join(" ")));
    }

    text
}

// 重导出子模块的公共项
pub use client_server::run_ui;
#[allow(unused_imports)]
pub(crate) use client_server::{
    ensure_selected_visible, is_secondary_register, next_visible_reg, prev_visible_reg,
};
pub use menu::load_profile_list;
#[allow(unused_imports)]
pub(crate) use menu::profile_monitor_mode_label;
pub(crate) use menu::{profile_pick_brief, run_menu};
#[allow(unused_imports)]
pub(crate) use monitor::{
    apply_pattern_dialog, format_monitor_history, format_monitor_stats, index_to_pattern,
    pattern_index, render_csv_picker, render_monitor_profile_pick,
};
