use anyhow::{anyhow, Context, Result};
use futures::StreamExt;
use tokio::sync::{mpsc, oneshot, RwLock};
use tokio::time::{timeout, Duration};

// std
use std::{borrow::Cow, collections::HashMap, io, sync::Arc};

// crossterm
use crossterm::{
    event::{Event, EventStream, KeyCode, KeyEvent, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};

// ratatui
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Paragraph, Row, Table, TableState, Wrap},
    Frame, Terminal,
};

// crate
use crate::{
    format_register_value, format_u16, modbus::frame_bytes_from_info, modbus::RegCmd,
    parse_u16_str, export_registers_to_json, AppState, Args, DisplayBase, FrameInfo,
    FrameRecord, MainMode, MonitorStats, RegDataFormat, BAR_HISTORY_SLOTS,
};
/// 从字符串解析寄存器数据格式
fn parse_reg_format(s: &str) -> RegDataFormat {
    match s.trim().to_lowercase().as_str() {
        "i16" => RegDataFormat::I16,
        "u32" | "uint32" => RegDataFormat::U32,
        "i32" | "int32" => RegDataFormat::I32,
        "u64" | "uint64" => RegDataFormat::U64,
        "i64" | "int64" => RegDataFormat::I64,
        "u128" | "uint128" => RegDataFormat::U128,
        "i128" | "int128" => RegDataFormat::I128,
        "f16" | "half" => RegDataFormat::F16,
        "f32" | "float" => RegDataFormat::F32,
        "f64" | "double" => RegDataFormat::F64,
        "bin" | "binary" => RegDataFormat::Binary,
        "ascii" => RegDataFormat::Ascii,
        _ => RegDataFormat::U16,
    }
}

const UI_TIMEOUT: Duration = Duration::from_secs(5);

/// 菜单屏幕状态
#[derive(Clone, Copy, PartialEq, Eq)]
enum MenuScreen {
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
    base: DisplayBase,
    selected: usize,
    scroll: usize,
    edit_mode: bool,
    edit_is_label: bool,
    edit_is_profile: bool,
    edit_buf: String,
    status_msg: Option<String>,
    show_byte_panel: bool,

    // --- 静默监听 ---
    show_monitor: bool,
    /// 历史记录滚动偏移
    monitor_scroll: usize,
    /// true=焦点在历史面板, false=焦点在统计面板
    monitor_focus_history: bool,

    // --- 菜单相关字段 ---
    /// 当前菜单屏幕：Main / ProfilePick / ProfileSet
    menu_screen: MenuScreen,
    /// ProfilePick/ProfileSet 以及主菜单中的选中项索引
    menu_list_idx: usize,
    /// 加载到的所有配置名
    profiles: Vec<String>,
    /// 当前选中的配置
    selected_profile: Option<String>,
    /// 待选模式（从主菜单进入子菜单时暂存）
    pending_mode: Option<MainMode>,
    /// 当前默认配置名
    default_profile: Option<String>,
    /// 主菜单渲染用的 Logo ASCII ART 行
    logo_lines: Vec<String>,
    /// Logo 动画帧计数器（递增到 30 后停止）
    logo_frame: u8,

    // --- 配置编辑相关字段 ---
    /// 正在编辑的配置名
    edit_profile_name: Option<String>,
    /// 正在编辑的配置参数副本
    edit_args: Option<Args>,
    /// 编辑字段缓冲
    field_edit_buf: String,
    /// 是否正在编辑字段值（true=编辑模式，false=导航模式）
    field_edit_mode: bool,
    /// 可用串口列表（启动时预检测并缓存）
    serial_ports: Vec<String>,
    /// 串口端口索引（用于设备字段编辑时循环选择）
    serial_port_idx: usize,

    // --- 新增/克隆配置相关字段 ---
    /// 新增/克隆时的名称输入缓冲
    name_prompt_buf: String,
    /// true=克隆选中配置, false=新建空配置
    name_prompt_is_clone: bool,
    /// 克隆时暂存的原配置 Args
    name_prompt_clone_args: Option<Args>,

    // --- 监听模式配置选择 ---
    /// 监听模式下是否正在选择配置
    monitor_picking: bool,
    /// 监听模式下已选中的配置名
    monitor_selected_profile: Option<String>,
    /// 筛选后的配置名列表（按 pending_mode 过滤）
    pick_names: Vec<String>,
    /// 筛选后的配置简介列表（与 pick_names 一一对应）
    pick_briefs: Vec<String>,
    /// 配置文件路径
    config_path: String,

    // --- 寄存器视图选择 ---
    /// 当前显示的寄存器类型：0=保持寄存器, 1=线圈, 2=离散输入, 3=输入寄存器
    reg_view: usize,

    // --- 协议分析对话框 ---
    /// 是否显示协议分析弹窗
    show_analysis_dialog: bool,
    /// 被分析的历史记录索引
    analysis_idx: usize,
    /// 从设备扫描结果对话框
    show_scan_dialog: bool,
    /// 寄存器变化模式配置对话框：是否打开
    pattern_dialog_open: bool,
    /// 正在配置的寄存器地址
    pattern_dialog_addr: usize,
    /// 寄存器类型 (REG_VIEW_HOLDING=0, REG_VIEW_INPUT=3)
    pattern_dialog_reg_type: usize,
    /// 当前选中的模式索引 (0=Random, 1=UpDown, 2=Sine, 3=Square, 4=Triangle)
    pattern_dialog_sel: usize,
    /// 是否正在编辑频率
    pattern_dialog_editing_freq: bool,
    /// 频率编辑缓存
    pattern_dialog_freq_buf: String,
    /// 临时存储的频率值
    pattern_dialog_freq: f64,
    /// 是否显示值变化历史条形图
    show_change_bar: bool,

    // --- 跳转寄存器地址 ---
    /// 是否正在输入目标寄存器地址
    goto_mode: bool,
    /// 地址输入缓存
    goto_buf: String,
    /// 搜索模式
    search_mode: bool,
    /// 搜索缓冲
    search_buf: String,
    /// 当前寄存器数据解释格式（UI 本地副本，与 AppState 同步）
    reg_format: RegDataFormat,
    /// 字节序交换
    swap_bytes: bool,
    /// 字序交换
    swap_words: bool,
}

/// 寄存器视图类型常量
const REG_VIEW_HOLDING: usize = 0;
const REG_VIEW_COILS: usize = 1;
const REG_VIEW_DISCRETE: usize = 2;
const REG_VIEW_INPUT: usize = 3;

impl Ui {
    fn new(base: DisplayBase, reg_format: RegDataFormat, swap_bytes: bool, swap_words: bool, profiles: Vec<String>) -> Self {
        let logo_raw = include_str!("logo.txt");
        let mut logo_lines: Vec<String> = logo_raw.lines().map(|l| l.to_string()).collect();
        logo_lines.push(format!("        v{}", env!("CARGO_PKG_VERSION")));
        let default = profiles.first().cloned();
        // 检测可用串口列表
        let serial_ports = tokio_serial::available_ports()
            .ok()
            .map(|ports| ports.into_iter().map(|p| p.port_name).collect())
            .unwrap_or_default();
        Self {
            base,
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
            logo_lines,
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
            swap_bytes,
            swap_words,
        }
    }
}

/// 获取当前寄存器视图的数据和可选标签
fn reg_view_data(s: &AppState, reg_view: usize) -> (&[u16], Option<&[String]>) {
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

/// 检查 register 是否匹配搜索词（按地址或标签）
fn search_match(idx: usize, search_lower: &str, _items: &[u16], labels: Option<&[String]>) -> bool {
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

/// 加载配置列表，返回所有配置名
pub fn load_profile_list(config_path: &str) -> Vec<String> {
    let config_str = std::fs::read_to_string(config_path).unwrap_or_default();
    let configs: HashMap<String, Args> = toml::from_str(&config_str).unwrap_or_default();
    let mut names: Vec<String> = configs.into_keys().collect();
    names.sort();
    names
}

/// 根据配置名加载配置，返回 (配置名, Args)
pub fn load_profile_args(config_path: &str, name: &str) -> Option<(String, Args)> {
    let config_str = std::fs::read_to_string(config_path).ok()?;
    let mut configs: HashMap<String, Args> = toml::from_str(&config_str).ok()?;
    configs.remove(name).map(|a| (name.to_string(), a))
}

/// 保存默认配置标记（写入到配置文件的 default 字段）
fn save_default_profile(config_path: &str, name: &str) -> Result<()> {
    let config_str = std::fs::read_to_string(config_path).unwrap_or_default();
    let mut configs: HashMap<String, Args> = toml::from_str(&config_str).unwrap_or_default();
    // 把默认配置名存为 key "default" 的特殊配置，只存一个 name 字段
    let default_args = Args {
        main_mode: name.to_string(),
        ..Default::default()
    };
    configs.insert("__default__".to_string(), default_args);
    let s = toml::to_string_pretty(&configs)?;
    std::fs::write(config_path, s)?;
    Ok(())
}

/// 读取默认配置名
fn load_default_profile(config_path: &str) -> Option<String> {
    let config_str = std::fs::read_to_string(config_path).ok()?;
    let configs: HashMap<String, Args> = toml::from_str(&config_str).ok()?;
    configs.get("__default__").map(|a| a.main_mode.clone())
}

// ─────────────────────────────────────────
// 菜单渲染与事件处理
// ─────────────────────────────────────────

/// 根据 Args 中的传输层类型返回监视模式标签
fn profile_monitor_mode_label(args: &Args) -> &'static str {
    if args.main_mode.to_ascii_lowercase().contains("tcp") {
        "tcp-monitor"
    } else {
        "rtu-monitor"
    }
}

/// 生成配置的单行简介文本
fn profile_pick_brief(args: &Args) -> String {
    let mode_short = match args.main_mode.to_ascii_lowercase().as_str() {
        "tcp-server" => "TCP-S",
        "tcp-client" => "TCP-C",
        "rtu-server" => "RTU-S",
        "rtu-client" => "RTU-C",
        _ => &args.main_mode,
    };
    let unit = format!("slv {}", args.unit);
    if args.main_mode.to_ascii_lowercase().contains("tcp") {
        format!(
            "{} | port {} | {} | hld {}",
            mode_short, args.tcp_port, unit, args.holding_count
        )
    } else {
        let parity = args.parity.to_uppercase();
        let baud = args.baudrate;
        format!(
            "{} | {} | {}-{}{}{} | {} | hld {}",
            mode_short,
            args.device,
            baud,
            parity,
            args.databits,
            args.stopbits,
            unit,
            args.holding_count
        )
    }
}

/// 读取配置文件，筛选出与 pending_mode 匹配的配置并生成简介
fn load_pick_list(config_path: &str, pending_mode: Option<MainMode>) -> (Vec<String>, Vec<String>) {
    let config_str = std::fs::read_to_string(config_path).unwrap_or_default();
    let configs: HashMap<String, Args> = toml::from_str(&config_str).unwrap_or_default();
    let mode_filter = pending_mode.map(|m| match m {
        MainMode::TcpServer => "tcp-server",
        MainMode::TcpClient => "tcp-client",
        MainMode::RTUServer => "rtu-server",
        MainMode::RTUClient => "rtu-client",
        MainMode::TcpMonitor => "tcp-",
        MainMode::RtuMonitor => "rtu-",
    });
    let mut entries: Vec<(String, String)> = configs
        .into_iter()
        .filter(|(name, _)| name != "__default__")
        .filter(|(_, args)| mode_filter.is_none_or(|f| args.main_mode.starts_with(f)))
        .map(|(name, args)| {
            let brief = profile_pick_brief(&args);
            (name, brief)
        })
        .collect();
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    let names: Vec<String> = entries.iter().map(|(n, _)| n.clone()).collect();
    let briefs: Vec<String> = entries.into_iter().map(|(_, b)| b).collect();
    (names, briefs)
}

/// 统计各模式下的配置数量（供主菜单显示）
fn count_profiles_by_mode(config_path: &str) -> [usize; 6] {
    let config_str = std::fs::read_to_string(config_path).unwrap_or_default();
    let configs: HashMap<String, Args> = toml::from_str(&config_str).unwrap_or_default();
    let mut counts = [0usize; 6];
    for (name, args) in &configs {
        if name == "__default__" {
            continue;
        }
        match args.main_mode.to_ascii_lowercase().as_str() {
            "tcp-server" => counts[0] += 1,
            "tcp-client" => counts[1] += 1,
            "rtu-server" => counts[2] += 1,
            "rtu-client" => counts[3] += 1,
            _ if args.main_mode.to_ascii_lowercase().contains("tcp") => counts[4] += 1,
            _ if args.main_mode.to_ascii_lowercase().contains("rtu") => counts[5] += 1,
            _ => {}
        }
    }
    counts
}

/// 主菜单：垂直排列，每个模式作为一个独立菜单项
fn render_main_menu(f: &mut Frame<'_>, ui: &Ui) {
    let area = f.area();
    let vert = Layout::vertical([
        Constraint::Length(7), // Logo + version
        Constraint::Min(12),   // 菜单项
        Constraint::Length(3), // 底部信息栏
    ])
    .split(area);

    // --- Logo 区（渐显动画） ---
    let logo_style = Style::default()
        .fg(match ui.logo_frame {
            0..=2 => Color::DarkGray,
            3..=5 => Color::Gray,
            6..=8 => Color::White,
            9..=11 => Color::LightCyan,
            12..=14 => Color::Cyan,
            _ => Color::Cyan,
        })
        .add_modifier(if ui.logo_frame >= 12 {
            Modifier::BOLD
        } else {
            Modifier::empty()
        });
    let logo_text: Vec<Line> = ui
        .logo_lines
        .iter()
        .map(|l| Line::from(Span::styled(l.clone(), logo_style)))
        .collect();
    f.render_widget(
        Paragraph::new(logo_text).alignment(ratatui::layout::Alignment::Center),
        vert[0],
    );

    // --- 主菜单区：垂直排列 ---
    let menu_items: [(&str, MainMode); 6] = [
        ("main_menu.tcp_server", MainMode::TcpServer),
        ("main_menu.tcp_client", MainMode::TcpClient),
        ("main_menu.rtu_server", MainMode::RTUServer),
        ("main_menu.rtu_client", MainMode::RTUClient),
        ("main_menu.tcp_monitor", MainMode::TcpMonitor),
        ("main_menu.rtu_monitor", MainMode::RtuMonitor),
    ];
    // 拉取配置列表统计各模式配置数
    let counts = count_profiles_by_mode(&ui.config_path);

    let block = Block::default()
        .borders(Borders::ALL)
        .title(t!("main_menu.title"))
        .border_style(Style::default().fg(Color::Cyan));

    let inner = block.inner(vert[1]);
    f.render_widget(block, vert[1]);

    let mut lines: Vec<Line> = Vec::new();

    // 渲染 5 个模式菜单项
    for (i, &(key, _mode)) in menu_items.iter().enumerate() {
        let is_selected = i == ui.menu_list_idx;
        let count = counts.get(i).copied().unwrap_or(0);
        let label = format!("[{}] {} ({})", i + 1, t!(key), count);
        let item_style = if is_selected {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        let prefix = if is_selected { " ▸ " } else { "   " };
        lines.push(Line::from(Span::styled(
            format!("{}{}", prefix, label),
            item_style,
        )));
        lines.push(Line::from(Span::raw(""))); // spacer
    }

    // 渲染 Profile Settings 项
    let i = menu_items.len();
    let is_selected = i == ui.menu_list_idx;
    let p_style = if is_selected {
        Style::default()
            .fg(Color::Black)
            .bg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    };
    let prefix = if is_selected { " ▸ " } else { "   " };
    lines.push(Line::from(Span::styled(
        format!("{}[7] {}", prefix, t!("main_menu.profile_settings")),
        p_style,
    )));

    let paragraph = Paragraph::new(lines).alignment(ratatui::layout::Alignment::Left);
    f.render_widget(paragraph, inner);

    // --- 底部信息栏 ---
    let default_name = ui
        .default_profile
        .as_deref()
        .map(std::borrow::Cow::Borrowed)
        .unwrap_or_else(|| t!("main_menu.none"));
    let selected = ui
        .selected_profile
        .as_deref()
        .map(std::borrow::Cow::Borrowed)
        .unwrap_or_else(|| t!("main_menu.none"));
    let status = t!(
        "main_menu.info_bar",
        default = default_name,
        selected = selected
    );
    f.render_widget(
        Paragraph::new(status)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(t!("main_menu.info")),
            )
            .style(Style::default().fg(Color::DarkGray)),
        vert[2],
    );
}

/// 渲染配置选择列表（从主菜单选择模式后选择配置）
fn render_profile_pick(f: &mut Frame<'_>, ui: &Ui, _config_path: &str) {
    let area = f.area();
    let vert = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(5),
        Constraint::Length(3),
    ])
    .split(area);

    let mode_name = ui
        .pending_mode
        .map(|m| match m {
            MainMode::TcpServer => "TCP Server",
            MainMode::TcpClient => "TCP Client",
            MainMode::RTUServer => "RTU Server",
            MainMode::RTUClient => "RTU Client",
            MainMode::TcpMonitor => "TCP Monitor",
            MainMode::RtuMonitor => "RTU Monitor",
        })
        .unwrap_or("?");
    let title = format!(
        "Select Profile — {mode_name} ({} profiles)",
        ui.pick_names.len()
    );
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            title,
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )))
        .alignment(ratatui::layout::Alignment::Center),
        vert[0],
    );

    let main =
        Layout::horizontal([Constraint::Percentage(55), Constraint::Percentage(45)]).split(vert[1]);

    // 左：筛选后的配置列表（含简介）
    let mut items: Vec<Line> = Vec::new();
    for (i, name) in ui.pick_names.iter().enumerate() {
        let is_default = Some(name.as_str()) == ui.default_profile.as_deref();
        let icon = if is_default { "●" } else { "○" };
        let brief = ui.pick_briefs.get(i).map(|s| s.as_str()).unwrap_or("");
        if i == ui.menu_list_idx {
            items.push(Line::from(Span::styled(
                format!(" {} {}", icon, name),
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )));
            items.push(Line::from(Span::styled(
                format!("   {}", brief),
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::DIM),
            )));
        } else if is_default {
            items.push(Line::from(Span::styled(
                format!(" {} {}", icon, name),
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            )));
            items.push(Line::from(Span::styled(
                format!("   {}", brief),
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::DIM),
            )));
        } else {
            items.push(Line::from(Span::styled(
                format!(" {} {}", icon, name),
                Style::default(),
            )));
            items.push(Line::from(Span::styled(
                format!("   {}", brief),
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::DIM),
            )));
        }
    }
    if ui.pick_names.is_empty() {
        items.push(Line::from(Span::styled(
            t!("profile_settings.empty_list"),
            Style::default().fg(Color::DarkGray),
        )));
        let empty_msg = if ui.profiles.is_empty() {
            t!("profile_pick.no_profiles_hint")
        } else {
            t!("profile_pick.no_match_hint")
        };
        items.push(Line::from(Span::styled(
            empty_msg,
            Style::default().fg(Color::DarkGray),
        )));
    }
    let list_block = Block::default()
        .borders(Borders::ALL)
        .title(t!("profile_settings.list_title"))
        .border_style(Style::default().fg(Color::Cyan));
    f.render_widget(Paragraph::new(items).block(list_block), main[0]);

    // 右：选中配置预览
    let right_content = if !ui.pick_names.is_empty() && ui.menu_list_idx < ui.pick_names.len() {
        let sel_name = &ui.pick_names[ui.menu_list_idx];
        let config_str = std::fs::read_to_string(_config_path).unwrap_or_default();
        let configs: HashMap<String, Args> = toml::from_str(&config_str).unwrap_or_default();
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
                    t!("profile_pick.preview_mode", mode = &args.main_mode)
                ),
                Style::default(),
            )));
            lines.push(Line::from(Span::styled(
                format!("  {}", t!("profile_pick.preview_unit", unit = args.unit)),
                Style::default(),
            )));
            lines.push(Line::from(Span::styled(
                format!(
                    "  {}",
                    t!("profile_pick.preview_count", count = args.holding_count)
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
            // coils / discrete / input
            lines.push(Line::from(Span::styled(
                format!(
                    "  coils:{} disc:{} input:{}",
                    args.coil_count, args.discrete_count, args.input_count
                ),
                Style::default().fg(Color::DarkGray),
            )));
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

    let help = t!("profile_pick.help");
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            help,
            Style::default().fg(Color::DarkGray),
        )))
        .alignment(ratatui::layout::Alignment::Center),
        vert[2],
    );
}

/// 渲染监听模式下的配置选择界面
fn render_monitor_profile_pick(f: &mut Frame<'_>, ui: &Ui, _config_path: &str) {
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
            let brief = configs
                .get(*n)
                .map(profile_pick_brief)
                .unwrap_or_default();
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
            lines.push(Line::from(Span::styled(
                format!("  {}", t!("profile_pick.preview_unit", unit = args.unit)),
                Style::default(),
            )));
            lines.push(Line::from(Span::styled(
                format!(
                    "  {}",
                    t!("profile_pick.preview_count", count = args.holding_count)
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
            // coils / discrete / input
            lines.push(Line::from(Span::styled(
                format!(
                    "  coils:{} disc:{} input:{}",
                    args.coil_count, args.discrete_count, args.input_count
                ),
                Style::default().fg(Color::DarkGray),
            )));
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

fn render_profile_settings(f: &mut Frame<'_>, ui: &Ui, _config_path: &str) {
    let area = f.area();
    let vert = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(10),
        Constraint::Length(6),
    ])
    .split(area);

    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            t!("profile_settings.title"),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )))
        .alignment(ratatui::layout::Alignment::Center),
        vert[0],
    );

    // 主体部分
    let main =
        Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)]).split(vert[1]);

    // 左：配置列表，可设置为默认
    let mut items: Vec<Line> = Vec::new();
    for (i, name) in ui.profiles.iter().enumerate() {
        let is_default = Some(name.as_str()) == ui.default_profile.as_deref();
        let icon = if is_default { "●" } else { "○" };
        let line = if i == ui.menu_list_idx {
            Line::from(Span::styled(
                format!(" {icon} {name}"),
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ))
        } else if is_default {
            Line::from(Span::styled(
                format!(" {icon} {name}"),
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ))
        } else {
            Line::from(Span::styled(format!(" {icon} {name}"), Style::default()))
        };
        items.push(line);
    }
    if ui.profiles.is_empty() {
        items.push(Line::from(Span::styled(
            t!("profile_settings.empty_list"),
            Style::default().fg(Color::DarkGray),
        )));
    }

    let list_block = Block::default()
        .borders(Borders::ALL)
        .title(t!("profile_settings.list_title"))
        .border_style(Style::default().fg(Color::Cyan));
    f.render_widget(Paragraph::new(items).block(list_block), main[0]);

    // 右：当前默认配置信息 + 操作说明
    let default_name = ui
        .default_profile
        .as_deref()
        .map(std::borrow::Cow::Borrowed)
        .unwrap_or_else(|| t!("profile_settings.info_default"));
    let info = t!("profile_settings.info_text", name = default_name);
    let info_block = Block::default()
        .borders(Borders::ALL)
        .title(t!("profile_settings.info_title"))
        .border_style(Style::default().fg(Color::Green));
    f.render_widget(Paragraph::new(info).block(info_block), main[1]);

    // 底部
    let help = t!("profile_settings.help");
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            help,
            Style::default().fg(Color::DarkGray),
        )))
        .alignment(ratatui::layout::Alignment::Center),
        vert[2],
    );
}

/// 处理主菜单的按键事件（垂直导航）
fn handle_main_menu_key(ui: &mut Ui, code: KeyCode, config_path: &str) -> Option<MenuSelection> {
    const ITEM_COUNT: usize = 7; // TCP Server(0), TCP Client(1), RTU Server(2), RTU Client(3), TCP Monitor(4), RTU Monitor(5), Profile Settings(6)

    fn enter_pick(ui: &mut Ui, config_path: &str, mode: MainMode) {
        ui.pending_mode = Some(mode);
        let (names, briefs) = load_pick_list(config_path, ui.pending_mode);
        ui.pick_names = names;
        ui.pick_briefs = briefs;
        ui.menu_screen = MenuScreen::ProfilePick;
        ui.menu_list_idx = 0;
    }

    match code {
        KeyCode::Up => {
            ui.menu_list_idx = ui.menu_list_idx.saturating_sub(1);
        }
        KeyCode::Down => {
            if ui.menu_list_idx + 1 < ITEM_COUNT {
                ui.menu_list_idx += 1;
            }
        }
        KeyCode::Char(c) if c.is_ascii_digit() => {
            let n = (c as u8 - b'1') as usize;
            match n {
                0 => enter_pick(ui, config_path, MainMode::TcpServer),
                1 => enter_pick(ui, config_path, MainMode::TcpClient),
                2 => enter_pick(ui, config_path, MainMode::RTUServer),
                3 => enter_pick(ui, config_path, MainMode::RTUClient),
                4 => enter_pick(ui, config_path, MainMode::TcpMonitor),
                5 => enter_pick(ui, config_path, MainMode::RtuMonitor),
                6 => {
                    ui.menu_screen = MenuScreen::ProfileSet;
                    ui.menu_list_idx = 0;
                }
                _ => {}
            }
        }
        KeyCode::Enter => match ui.menu_list_idx {
            0 => enter_pick(ui, config_path, MainMode::TcpServer),
            1 => enter_pick(ui, config_path, MainMode::TcpClient),
            2 => enter_pick(ui, config_path, MainMode::RTUServer),
            3 => enter_pick(ui, config_path, MainMode::RTUClient),
            4 => enter_pick(ui, config_path, MainMode::TcpMonitor),
            5 => enter_pick(ui, config_path, MainMode::RtuMonitor),
            6 => {
                // Profile Settings
                ui.menu_screen = MenuScreen::ProfileSet;
                ui.menu_list_idx = 0;
            }
            _ => {}
        },
        KeyCode::Char('q') => {
            // 退出程序
            return Some(MenuSelection {
                main_mode: MainMode::TcpClient,
                profile_name: None,
                quit: true,
            });
        }
        _ => {}
    }
    None
}

/// 处理配置选择子菜单的按键事件
fn handle_profile_pick_key(
    ui: &mut Ui,
    code: KeyCode,
    _config_path: &str,
) -> Option<MenuSelection> {
    match code {
        KeyCode::Up => {
            if !ui.pick_names.is_empty() {
                ui.menu_list_idx = ui.menu_list_idx.saturating_sub(1);
            }
        }
        KeyCode::Down => {
            if ui.menu_list_idx + 1 < ui.pick_names.len() {
                ui.menu_list_idx += 1;
            }
        }
        KeyCode::Enter => {
            let mode = ui.pending_mode.unwrap_or(MainMode::TcpClient);
            let profile = if ui.menu_list_idx < ui.pick_names.len() {
                Some(ui.pick_names[ui.menu_list_idx].clone())
            } else {
                None
            };
            ui.selected_profile = profile.clone();
            return Some(MenuSelection {
                main_mode: mode,
                profile_name: profile,
                quit: false,
            });
        }
        KeyCode::Esc => {
            ui.menu_screen = MenuScreen::Main;
        }
        KeyCode::Char('q') => {
            return Some(MenuSelection {
                main_mode: MainMode::TcpClient,
                profile_name: None,
                quit: true,
            });
        }
        _ => {}
    }
    None
}

/// 处理配置管理设置的按键事件
fn handle_profile_set_key(ui: &mut Ui, code: KeyCode, config_path: &str) -> Option<MenuSelection> {
    match code {
        KeyCode::Up => {
            if !ui.profiles.is_empty() {
                ui.menu_list_idx = ui.menu_list_idx.saturating_sub(1);
            }
        }
        KeyCode::Down => {
            if ui.menu_list_idx + 1 < ui.profiles.len() {
                ui.menu_list_idx += 1;
            }
        }
        KeyCode::Enter => {
            if ui.menu_list_idx < ui.profiles.len() {
                let name = ui.profiles[ui.menu_list_idx].clone();
                if save_default_profile(config_path, &name).is_ok() {
                    ui.default_profile = Some(name.clone());
                    ui.selected_profile = Some(name);
                    set_status(
                        ui,
                        t!(
                            "profile_settings.set_success",
                            name = ui.default_profile.as_deref().unwrap_or("")
                        ),
                    );
                } else {
                    set_status(ui, t!("profile_settings.set_fail"));
                }
            }
        }
        KeyCode::Char('e') => {
            // 编辑选中配置
            if ui.menu_list_idx < ui.profiles.len() {
                let name = ui.profiles[ui.menu_list_idx].clone();
                if let Some((_, args)) = load_profile_args(config_path, &name) {
                    ui.edit_profile_name = Some(name.clone());
                    ui.edit_args = Some(args);
                    ui.menu_list_idx = 0;
                    ui.field_edit_mode = false;
                    ui.field_edit_buf.clear();
                    ui.menu_screen = MenuScreen::ProfileEdit;
                } else {
                    set_status(ui, t!("profile_settings.load_fail"));
                }
            }
        }
        KeyCode::Char('a') => {
            // 新增配置
            ui.name_prompt_buf.clear();
            ui.name_prompt_is_clone = false;
            ui.name_prompt_clone_args = None;
            ui.menu_screen = MenuScreen::NamePrompt;
        }
        KeyCode::Char('c') => {
            // 克隆选中配置
            if ui.menu_list_idx < ui.profiles.len() {
                let name = ui.profiles[ui.menu_list_idx].clone();
                if let Some((_, args)) = load_profile_args(config_path, &name) {
                    ui.name_prompt_buf.clear();
                    ui.name_prompt_is_clone = true;
                    ui.name_prompt_clone_args = Some(args);
                    ui.menu_screen = MenuScreen::NamePrompt;
                } else {
                    set_status(ui, t!("profile_settings.load_fail"));
                }
            }
        }
        KeyCode::Char('d') => {
            // 删除选中配置（需两次确认）
            if ui.menu_list_idx < ui.profiles.len() {
                let name = ui.profiles[ui.menu_list_idx].clone();
                let confirm_key = t!("profile_settings.delete_confirm", name = &name);
                let is_confirming = ui.status_msg.as_deref() == Some(confirm_key.as_ref());
                if is_confirming {
                    // 第二次按 d：执行删除
                    let config_str = std::fs::read_to_string(config_path).unwrap_or_default();
                    let mut configs: HashMap<String, Args> =
                        toml::from_str(&config_str).unwrap_or_default();
                    configs.remove(&name);
                    if let Ok(s) = toml::to_string_pretty(&configs) {
                        if std::fs::write(config_path, s).is_ok() {
                            // 重新加载列表
                            let mut reloaded = load_profile_list(config_path);
                            reloaded.sort();
                            ui.profiles = reloaded;
                            ui.menu_list_idx =
                                ui.menu_list_idx.min(ui.profiles.len().saturating_sub(1));
                            // 如果删除的是默认配置，清除默认
                            if ui.default_profile.as_deref() == Some(&name) {
                                ui.default_profile = None;
                                let _ = save_default_profile(config_path, "");
                            }
                            set_status(ui, t!("profile_settings.delete_success", name = &name));
                        } else {
                            set_status(ui, t!("profile_settings.delete_fail"));
                        }
                    } else {
                        set_status(ui, t!("profile_settings.delete_fail"));
                    }
                } else {
                    // 第一次按 d：显示确认提示
                    set_status(ui, confirm_key);
                }
            }
        }
        KeyCode::Esc => {
            ui.menu_screen = MenuScreen::Main;
        }
        KeyCode::Char('q') => {
            return Some(MenuSelection {
                main_mode: MainMode::TcpClient,
                profile_name: None,
                quit: true,
            });
        }
        _ => {}
    }
    None
}

// ─────────────────────────────────────────
// 配置字段编辑：字段定义、渲染、按键处理
// ─────────────────────────────────────────

type FieldApply = Box<dyn Fn(&mut Args, &str) -> Result<(), String>>;
type FieldDisplay = Box<dyn Fn(&Args) -> String>;

/// 可编辑的配置字段描述
#[allow(clippy::type_complexity)]
struct ProfileField {
    /// 字段名（i18n key 后缀）
    name_key: &'static str,
    /// 字段适用模式: "tcp", "rtu", "all"
    mode: &'static str,
    /// 从 Args 中提取当前值的显示字符串
    display: Box<dyn Fn(&Args) -> String>,
    /// 将字符串解析为新值并设置到 Args 中
    apply: Box<dyn Fn(&mut Args, &str) -> Result<(), String>>,
}

/// 返回所有可编辑字段的列表
fn profile_fields() -> Vec<ProfileField> {
    fn main_mode_display(a: &Args) -> String {
        a.main_mode.clone()
    }
    fn main_mode_apply(a: &mut Args, v: &str) -> Result<(), String> {
        let v = v.trim().to_lowercase();
        match v.as_str() {
            "tcp-server" | "ts" => a.main_mode = "tcp-server".into(),
            "tcp-client" | "tc" => a.main_mode = "tcp-client".into(),
            "tcp-monitor" | "tm" => a.main_mode = "tcp-monitor".into(),
            "rtu-server" | "rs" => a.main_mode = "rtu-server".into(),
            "rtu-client" | "rc" => a.main_mode = "rtu-client".into(),
            "rtu-monitor" | "rm" => a.main_mode = "rtu-monitor".into(),
            _ => return Err(format!("无效模式: {v} (ts/tc/tm/rs/rc/rm)")),
        }
        Ok(())
    }
    fn num_display<T: ToString + 'static>(f: fn(&Args) -> T) -> FieldDisplay {
        Box::new(move |a| f(a).to_string())
    }
    fn num_apply<T: std::str::FromStr + 'static>(
        set: fn(&mut Args, T),
        name: &'static str,
    ) -> FieldApply {
        Box::new(move |a, v| {
            let v = v.trim();
            v.parse::<T>()
                .map(|val| set(a, val))
                .map_err(|_| format!("{name} 必须是有效数字"))
        })
    }
    fn str_apply(set: fn(&mut Args, String)) -> FieldApply {
        Box::new(move |a, v| {
            set(a, v.to_string());
            Ok(())
        })
    }

    vec![
        ProfileField {
            name_key: "main_mode",
            mode: "all",
            display: Box::new(main_mode_display),
            apply: Box::new(main_mode_apply),
        },
        ProfileField {
            name_key: "tcp_port",
            mode: "tcp",
            display: num_display(|a: &Args| a.tcp_port),
            apply: num_apply(|a: &mut Args, v| a.tcp_port = v, "TCP端口"),
        },
        ProfileField {
            name_key: "unit",
            mode: "all",
            display: num_display(|a: &Args| a.unit),
            apply: num_apply(|a: &mut Args, v| a.unit = v, "从站地址"),
        },
        ProfileField {
            name_key: "holding_count",
            mode: "all",
            display: num_display(|a: &Args| a.holding_count),
            apply: num_apply(|a: &mut Args, v| a.holding_count = v, "寄存器数"),
        },
        ProfileField {
            name_key: "client_tick_ms",
            mode: "all",
            display: num_display(|a: &Args| a.client_tick_ms),
            apply: num_apply(|a: &mut Args, v| a.client_tick_ms = v, "轮询间隔"),
        },
        ProfileField {
            name_key: "device",
            mode: "rtu",
            display: Box::new(|a: &Args| a.device.clone()),
            apply: str_apply(|a: &mut Args, v| a.device = v),
        },
        ProfileField {
            name_key: "baudrate",
            mode: "rtu",
            display: num_display(|a: &Args| a.baudrate),
            apply: num_apply(|a: &mut Args, v| a.baudrate = v, "波特率"),
        },
        ProfileField {
            name_key: "parity",
            mode: "rtu",
            display: Box::new(|a: &Args| a.parity.clone()),
            apply: Box::new(|a: &mut Args, v: &str| {
                let v = v.trim().to_lowercase();
                match v.as_str() {
                    "n" | "none" => a.parity = "n".into(),
                    "e" | "even" => a.parity = "e".into(),
                    "o" | "odd" => a.parity = "o".into(),
                    _ => return Err(format!("无效校验位: {v} (n/e/o)")),
                }
                Ok(())
            }),
        },
        ProfileField {
            name_key: "flow",
            mode: "rtu",
            display: Box::new(|a: &Args| a.flow.clone()),
            apply: Box::new(|a: &mut Args, v: &str| {
                let v = v.trim().to_lowercase();
                match v.as_str() {
                    "none" => a.flow = "none".into(),
                    "hardware" | "hard" => a.flow = "hardware".into(),
                    "software" | "soft" => a.flow = "software".into(),
                    _ => return Err(format!("无效流控: {v} (none/hard/soft)")),
                }
                Ok(())
            }),
        },
        ProfileField {
            name_key: "databits",
            mode: "rtu",
            display: num_display(|a: &Args| a.databits),
            apply: Box::new(|a: &mut Args, v: &str| {
                let val: u8 = v
                    .trim()
                    .parse()
                    .map_err(|_| "数据位必须是数字".to_string())?;
                if ![5, 6, 7, 8].contains(&val) {
                    return Err("数据位必须是 5/6/7/8".to_string());
                }
                a.databits = val;
                Ok(())
            }),
        },
        ProfileField {
            name_key: "stopbits",
            mode: "rtu",
            display: num_display(|a: &Args| a.stopbits),
            apply: Box::new(|a: &mut Args, v: &str| {
                let val: u8 = v
                    .trim()
                    .parse()
                    .map_err(|_| "停止位必须是数字".to_string())?;
                if val != 1 && val != 2 {
                    return Err("停止位必须是 1 或 2".to_string());
                }
                a.stopbits = val;
                Ok(())
            }),
        },
    ]
}

/// 保存编辑后的配置到配置文件
fn save_edited_profile(config_path: &str, profile_name: &str, args: &Args) -> Result<()> {
    let config_str = std::fs::read_to_string(config_path).unwrap_or_default();
    let mut configs: HashMap<String, Args> = toml::from_str(&config_str).unwrap_or_default();
    configs.insert(profile_name.to_string(), args.clone());
    let s = toml::to_string_pretty(&configs)?;
    std::fs::write(config_path, s)?;
    Ok(())
}

/// 渲染配置编辑界面
fn render_profile_edit(f: &mut Frame<'_>, ui: &Ui, _config_path: &str) {
    let area = f.area();
    let vert = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(10),
        Constraint::Length(6),
    ])
    .split(area);

    let profile_name = ui.edit_profile_name.as_deref().unwrap_or("?");
    let title = t!("profile_edit.title", name = profile_name);
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

    let fields = profile_fields();
    let main =
        Layout::horizontal([Constraint::Percentage(55), Constraint::Percentage(45)]).split(vert[1]);

    // 左：字段列表（含模式标签）
    let mut lines: Vec<Line> = Vec::new();
    for (i, field) in fields.iter().enumerate() {
        let is_selected = i == ui.menu_list_idx;
        let args = ui.edit_args.as_ref().unwrap();
        let val = (field.display)(args);
        let label_key = format!("profile_edit.{}", field.name_key);
        let label = t!(&label_key);
        let mode_tag = match field.mode {
            "tcp" => t!("profile_edit.field_mode_tcp"),
            "rtu" => t!("profile_edit.field_mode_rtu"),
            _ => t!("profile_edit.field_mode_all"),
        };

        let line = if is_selected {
            Line::from(vec![
                Span::styled(" ▸ ", Style::default().fg(Color::Yellow)),
                Span::styled(
                    format!("{}: ", label),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::styled(val, Style::default().fg(Color::Black).bg(Color::Cyan)),
                Span::raw(" "),
                Span::styled(
                    mode_tag,
                    Style::default()
                        .fg(Color::DarkGray)
                        .add_modifier(Modifier::DIM),
                ),
            ])
        } else {
            Line::from(vec![
                Span::raw("   "),
                Span::styled(format!("{}: ", label), Style::default()),
                Span::styled(val, Style::default().fg(Color::Green)),
                Span::raw(" "),
                Span::styled(
                    mode_tag,
                    Style::default()
                        .fg(Color::DarkGray)
                        .add_modifier(Modifier::DIM),
                ),
            ])
        };
        lines.push(line);
    }

    let list_block = Block::default()
        .borders(Borders::ALL)
        .title(t!("profile_edit.fields_title"))
        .border_style(Style::default().fg(Color::Cyan));
    f.render_widget(
        Paragraph::new(lines)
            .block(list_block)
            .wrap(Wrap { trim: false }),
        main[0],
    );

    // 右：编辑面板或提示
    let right_content = if ui.field_edit_mode {
        // 编辑模式下显示输入框
        let field = &fields[ui.menu_list_idx];
        let label_key = format!("profile_edit.{}", field.name_key);
        let label = t!(&label_key);
        let mut edit_lines = vec![
            Line::from(Span::styled(
                t!("profile_edit.editing", label = label.as_ref()),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::raw("")),
            Line::from(Span::styled(
                format!("> {}", ui.field_edit_buf),
                Style::default().fg(Color::Cyan),
            )),
            Line::from(Span::raw("")),
        ];

        // 如果是 device 字段编辑，显示可用串口列表
        if field.name_key == "device" && !ui.serial_ports.is_empty() {
            edit_lines.push(Line::from(Span::styled(
                t!("profile_edit.device_port_list"),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )));
            for port in ui.serial_ports.iter() {
                let selected_port = &ui.serial_ports[ui.serial_port_idx % ui.serial_ports.len()];
                if port == selected_port {
                    edit_lines.push(Line::from(vec![
                        Span::styled(" ● ", Style::default().fg(Color::Cyan)),
                        Span::styled(
                            port.clone(),
                            Style::default()
                                .fg(Color::Cyan)
                                .add_modifier(Modifier::BOLD),
                        ),
                    ]));
                } else {
                    edit_lines.push(Line::from(vec![
                        Span::raw(" ○ "),
                        Span::styled(port.clone(), Style::default().fg(Color::DarkGray)),
                    ]));
                }
            }
            edit_lines.push(Line::from(Span::raw("")));
            edit_lines.push(Line::from(Span::styled(
                t!("profile_edit.serial_hint"),
                Style::default().fg(Color::DarkGray),
            )));
        } else if field.name_key == "device" && ui.serial_ports.is_empty() {
            edit_lines.push(Line::from(Span::styled(
                t!("profile_edit.device_no_ports"),
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::DIM),
            )));
            edit_lines.push(Line::from(Span::raw("")));
        }

        // 如有校验提示，添加提示信息
        let hint = match field.name_key {
            "main_mode" => "ts/tc/tm/rs/rc/rm 或 tcp-client/tcp-server/tcp-monitor/rtu-client/rtu-server/rtu-monitor",
            "parity" => "n(even) / e(ven) / o(dd)",
            "flow" => "none / hard(ware) / soft(ware)",
            "databits" => "5 / 6 / 7 / 8",
            "stopbits" => "1 / 2",
            _ => "",
        };
        if !hint.is_empty() {
            edit_lines.push(Line::from(Span::styled(
                hint,
                Style::default().fg(Color::DarkGray),
            )));
            edit_lines.push(Line::from(Span::raw("")));
        }
        edit_lines.push(Line::from(Span::styled(
            t!("profile_edit.edit_help"),
            Style::default().fg(Color::DarkGray),
        )));
        Paragraph::new(edit_lines).wrap(Wrap { trim: false })
    } else {
        // 导航模式显示提示
        let help_text = if ui.edit_args.is_some() {
            t!("profile_edit.nav_help")
        } else {
            Cow::from("")
        };
        let mut right_lines = vec![
            Line::from(Span::styled(
                t!("profile_edit.preview_title"),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::raw("")),
        ];
        if let Some(ref args) = ui.edit_args {
            let summary = fields
                .iter()
                .map(|f| {
                    let key = format!("profile_edit.{}", f.name_key);
                    format!("  {}: {}", t!(&key), (f.display)(args))
                })
                .collect::<Vec<_>>()
                .join("\n");
            right_lines.push(Line::from(Span::styled(
                summary,
                Style::default().fg(Color::DarkGray),
            )));
        }
        right_lines.push(Line::from(Span::raw("")));
        right_lines.push(Line::from(Span::styled(
            help_text,
            Style::default().fg(Color::DarkGray),
        )));
        Paragraph::new(right_lines).wrap(Wrap { trim: false })
    };

    let edit_block = Block::default()
        .borders(Borders::ALL)
        .title(t!("profile_edit.edit_title"))
        .border_style(Style::default().fg(Color::Green));
    f.render_widget(right_content.block(edit_block), main[1]);

    // 底部帮助
    let help = t!("profile_edit.help");
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            help,
            Style::default().fg(Color::DarkGray),
        )))
        .wrap(Wrap { trim: false })
        .alignment(ratatui::layout::Alignment::Center),
        vert[2],
    );
}

/// 处理配置编辑界面的按键事件
fn handle_profile_edit_key(ui: &mut Ui, code: KeyCode, config_path: &str) -> Option<MenuSelection> {
    let fields = profile_fields();
    let field_count = fields.len();

    if ui.field_edit_mode {
        // 字段值编辑模式
        match code {
            KeyCode::Esc => {
                ui.field_edit_mode = false;
                ui.field_edit_buf.clear();
                set_status(ui, t!("profile_edit.edit_cancelled"));
            }
            KeyCode::Enter => {
                let idx = ui.menu_list_idx.min(field_count.saturating_sub(1));
                let val = ui.field_edit_buf.clone();
                if let Some(ref mut args) = ui.edit_args {
                    match (fields[idx].apply)(args, &val) {
                        Ok(()) => {
                            ui.field_edit_mode = false;
                            ui.field_edit_buf.clear();
                            set_status(ui, t!("profile_edit.field_updated"));
                        }
                        Err(msg) => {
                            set_status(ui, msg);
                        }
                    }
                }
            }
            KeyCode::Backspace => {
                ui.field_edit_buf.pop();
            }
            KeyCode::Tab | KeyCode::Down => {
                // 在 device 字段编辑时，Tab/↓ 切换到下一个可用串口
                let idx = ui.menu_list_idx.min(field_count.saturating_sub(1));
                if fields[idx].name_key == "device" && !ui.serial_ports.is_empty() {
                    let len = ui.serial_ports.len();
                    ui.serial_port_idx = (ui.serial_port_idx + 1) % len;
                    ui.field_edit_buf = ui.serial_ports[ui.serial_port_idx].clone();
                }
            }
            KeyCode::Up => {
                // 在 device 字段编辑时，↑ 切换到上一个可用串口
                let idx = ui.menu_list_idx.min(field_count.saturating_sub(1));
                if fields[idx].name_key == "device" && !ui.serial_ports.is_empty() {
                    let len = ui.serial_ports.len();
                    ui.serial_port_idx = (ui.serial_port_idx + len - 1) % len;
                    ui.field_edit_buf = ui.serial_ports[ui.serial_port_idx].clone();
                }
            }
            KeyCode::Char(ch) => {
                ui.field_edit_buf.push(ch);
            }
            _ => {}
        }
    } else {
        // 导航模式
        match code {
            KeyCode::Up => {
                ui.menu_list_idx = ui.menu_list_idx.saturating_sub(1);
            }
            KeyCode::Down => {
                if ui.menu_list_idx + 1 < field_count {
                    ui.menu_list_idx += 1;
                }
            }
            KeyCode::Enter => {
                // 进入编辑模式
                let idx = ui.menu_list_idx.min(field_count.saturating_sub(1));
                let val = (fields[idx].display)(ui.edit_args.as_ref().unwrap());
                ui.field_edit_buf = val;
                ui.field_edit_mode = true;
            }
            KeyCode::Char('s') => {
                // 保存配置
                let name = ui.edit_profile_name.clone();
                if let Some(ref args) = ui.edit_args {
                    if let Some(ref name) = name {
                        match save_edited_profile(config_path, name, args) {
                            Ok(()) => {
                                set_status(ui, t!("profile_edit.save_success", name = name));
                                // 回退到配置管理
                                ui.menu_screen = MenuScreen::ProfileSet;
                                ui.menu_list_idx =
                                    ui.profiles.iter().position(|p| p == name).unwrap_or(0);
                                ui.edit_profile_name = None;
                                ui.edit_args = None;
                            }
                            Err(e) => {
                                set_status(ui, format!("{}", e));
                            }
                        }
                    }
                }
            }
            KeyCode::Esc => {
                // 返回配置管理，不保存
                ui.menu_screen = MenuScreen::ProfileSet;
                ui.menu_list_idx = ui
                    .profiles
                    .iter()
                    .position(|p| Some(p.as_str()) == ui.edit_profile_name.as_deref())
                    .unwrap_or(0);
                ui.edit_profile_name = None;
                ui.edit_args = None;
                set_status(ui, t!("profile_edit.edit_cancelled"));
            }
            KeyCode::Char('q') => {
                return Some(MenuSelection {
                    main_mode: MainMode::TcpClient,
                    profile_name: None,
                    quit: true,
                });
            }
            _ => {}
        }
    }
    None
}

// ─────────────────────────────────────────
// 名称输入提示：新增/克隆配置
// ─────────────────────────────────────────

/// 保存一个新配置到配置文件
fn save_new_profile(config_path: &str, name: &str, args: &Args) -> Result<()> {
    let config_str = std::fs::read_to_string(config_path).unwrap_or_default();
    let mut configs: HashMap<String, Args> = toml::from_str(&config_str).unwrap_or_default();
    configs.insert(name.to_string(), args.clone());
    let s = toml::to_string_pretty(&configs)?;
    std::fs::write(config_path, s)?;
    Ok(())
}

/// 渲染名称输入界面
fn render_name_prompt(f: &mut Frame<'_>, ui: &Ui) {
    let area = f.area();
    let vert = Layout::vertical([
        Constraint::Length(3),
        Constraint::Length(7),
        Constraint::Min(0),
    ])
    .split(area);

    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            if ui.name_prompt_is_clone {
                t!("profile_settings.clone_title")
            } else {
                t!("profile_settings.add_title")
            },
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )))
        .alignment(ratatui::layout::Alignment::Center),
        vert[0],
    );

    let prompt_block = Block::default()
        .borders(Borders::ALL)
        .title(t!("profile_settings.name_prompt"))
        .border_style(Style::default().fg(Color::Cyan));
    let input_text: Line = if ui.name_prompt_buf.is_empty() {
        Line::from(Span::styled(
            t!("profile_settings.name_placeholder"),
            Style::default(),
        ))
    } else {
        Line::from(Span::styled(ui.name_prompt_buf.as_str(), Style::default()))
    };
    f.render_widget(
        Paragraph::new(input_text)
            .block(prompt_block)
            .alignment(ratatui::layout::Alignment::Center),
        vert[1],
    );

    let help = t!("profile_settings.name_help");
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            help,
            Style::default().fg(Color::DarkGray),
        )))
        .alignment(ratatui::layout::Alignment::Center),
        vert[2],
    );
}

/// 处理名称输入界面的按键事件
fn handle_name_prompt_key(ui: &mut Ui, code: KeyCode, config_path: &str) -> Option<MenuSelection> {
    match code {
        KeyCode::Enter => {
            let name = ui.name_prompt_buf.trim().to_string();
            if name.is_empty() {
                set_status(ui, t!("profile_settings.name_empty"));
                return None;
            }
            if ui.profiles.contains(&name) {
                set_status(ui, t!("profile_settings.name_exists"));
                return None;
            }
            if name == "__default__" {
                set_status(ui, t!("profile_settings.name_reserved"));
                return None;
            }
            let args = if ui.name_prompt_is_clone {
                ui.name_prompt_clone_args.clone().unwrap_or_default()
            } else {
                Args::default()
            };
            match save_new_profile(config_path, &name, &args) {
                Ok(()) => {
                    set_status(ui, t!("profile_settings.add_success", name = &name));
                    // 重新加载配置列表
                    let mut reloaded = load_profile_list(config_path);
                    reloaded.sort();
                    ui.profiles = reloaded;
                    // 选中新配置并进入编辑模式
                    ui.edit_profile_name = Some(name.clone());
                    ui.edit_args = Some(args);
                    ui.menu_list_idx = 0;
                    ui.field_edit_mode = false;
                    ui.field_edit_buf.clear();
                    ui.menu_screen = MenuScreen::ProfileEdit;
                }
                Err(e) => {
                    set_status(ui, format!("{}", e));
                }
            }
        }
        KeyCode::Esc => {
            ui.menu_screen = MenuScreen::ProfileSet;
        }
        KeyCode::Backspace => {
            ui.name_prompt_buf.pop();
        }
        KeyCode::Char('q') => {
            return Some(MenuSelection {
                main_mode: MainMode::TcpClient,
                profile_name: None,
                quit: true,
            });
        }
        KeyCode::Char(ch) => {
            if ch.is_alphanumeric() || ch == '_' || ch == '-' {
                ui.name_prompt_buf.push(ch);
            }
        }
        _ => {}
    }
    None
}

/// 运行主菜单界面，返回用户选择的结果
pub async fn run_menu(config_path: &str, profiles: Vec<String>) -> Result<MenuSelection> {
    enable_raw_mode().context("enable raw mode")?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen).context("enter alt screen")?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).context("create terminal")?;

    let mut events = EventStream::new();
    let mut ui = Ui::new(DisplayBase::Dec, RegDataFormat::U16, false, false, profiles);
    ui.config_path = config_path.to_string();
    // 尝试读取默认配置
    if let Some(def) = load_default_profile(config_path) {
        if ui.profiles.contains(&def) {
            ui.default_profile = Some(def.clone());
            ui.selected_profile = Some(def);
        }
    }

    let res: Result<MenuSelection> = loop {
        let ev = tokio::time::timeout(Duration::from_millis(100), events.next()).await;
        let ev = match ev {
            Ok(Some(Ok(ev))) => ev,
            Ok(Some(Err(e))) => break Err(anyhow!(e).context("read event")),
            _ => {
                // Logo 动画（约 3 秒完成，30 frames × 100ms）
                if ui.logo_frame < 30 {
                    ui.logo_frame += 1;
                }
                // 刷新界面
                let _ = terminal.draw(|f| match ui.menu_screen {
                    MenuScreen::Main => render_main_menu(f, &ui),
                    MenuScreen::ProfilePick => render_profile_pick(f, &ui, config_path),
                    MenuScreen::ProfileSet => render_profile_settings(f, &ui, config_path),
                    MenuScreen::ProfileEdit => render_profile_edit(f, &ui, config_path),
                    MenuScreen::NamePrompt => render_name_prompt(f, &ui),
                });
                continue;
            }
        };

        match ev {
            Event::Key(KeyEvent { code, kind, .. }) => {
                if kind != crossterm::event::KeyEventKind::Press {
                    continue;
                }
                let sel = match ui.menu_screen {
                    MenuScreen::Main => handle_main_menu_key(&mut ui, code, config_path),
                    MenuScreen::ProfilePick => handle_profile_pick_key(&mut ui, code, config_path),
                    MenuScreen::ProfileSet => handle_profile_set_key(&mut ui, code, config_path),
                    MenuScreen::ProfileEdit => handle_profile_edit_key(&mut ui, code, config_path),
                    MenuScreen::NamePrompt => handle_name_prompt_key(&mut ui, code, config_path),
                };
                if let Some(sel) = sel {
                    break Ok(sel);
                }
            }
            Event::Resize(_, _) => {
                let _ = terminal.draw(|f| match ui.menu_screen {
                    MenuScreen::Main => render_main_menu(f, &ui),
                    MenuScreen::ProfilePick => render_profile_pick(f, &ui, config_path),
                    MenuScreen::ProfileSet => render_profile_settings(f, &ui, config_path),
                    MenuScreen::ProfileEdit => render_profile_edit(f, &ui, config_path),
                    MenuScreen::NamePrompt => render_name_prompt(f, &ui),
                });
            }
            _ => {}
        }

        // 重绘
        let _ = terminal.draw(|f| match ui.menu_screen {
            MenuScreen::Main => render_main_menu(f, &ui),
            MenuScreen::ProfilePick => render_profile_pick(f, &ui, config_path),
            MenuScreen::ProfileSet => render_profile_settings(f, &ui, config_path),
            MenuScreen::ProfileEdit => render_profile_edit(f, &ui, config_path),
            MenuScreen::NamePrompt => render_name_prompt(f, &ui),
        });
    };

    disable_raw_mode().ok();
    execute!(terminal.backend_mut(), LeaveAlternateScreen).ok();
    terminal.show_cursor().ok();
    res
}

pub async fn run_ui(
    state: Arc<RwLock<AppState>>,
    tx: mpsc::UnboundedSender<RegCmd>,
    args: Args,
    server_status: Arc<RwLock<Option<String>>>,
    config_path: String,
    profiles: Vec<String>,
) -> Result<()> {
    enable_raw_mode().context(t!("run_ui.enable_raw_mode"))?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen).context(t!("run_ui.enter_alt_screen"))?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).context(t!("run_ui.create_terminal"))?;

    let mut events = EventStream::new();
    let reg_format = parse_reg_format(&args.reg_format);
    let swap_bytes = args.swap_bytes;
    let swap_words = args.swap_words;
    let mut ui = Ui::new(args.base, reg_format, swap_bytes, swap_words, profiles);
    ui.config_path = config_path;

    // Monitor 模式默认开启监听面板
    if args.main_mode.contains("monitor") {
        ui.show_monitor = true;
    }

    let tick = Duration::from_millis(args.ui_tick_ms);
    let mut interval = tokio::time::interval(tick);

    let res: Result<()> = loop {
        tokio::select! {
                    _ = interval.tick() => {
                        let s = state.read().await;
                        // 从 AppState 同步格式/交换设置到 UI（允许程序化修改）
                        ui.reg_format = s.reg_format;
                        ui.swap_bytes = s.swap_bytes;
                        ui.swap_words = s.swap_words;
                        let server_err = server_status.read().await.clone();
                        let is_monitor_mode = args.main_mode == "monitor";
                        terminal.draw(|f| {
                            let monitor_active = is_monitor_mode || ui.show_monitor;

                            // --- 预计算帮助文本（用于动态高度和值变化状态指示） ---
                            let mut help = if ui.edit_mode {
                                t!("run_ui.help_edit", buf = &ui.edit_buf).into_owned()
                            } else if s.stability_test_running {
                                t!("run_ui.help_stability").into_owned()
                            } else if is_monitor_mode {
                                t!("run_ui.help_monitoring").into_owned()
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
                                    t!("run_ui.edit_value", base = format!("{:?}", ui.base), buf = &ui.edit_buf)
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
                                let sw = if ui.swap_bytes || ui.swap_words {
                                    format!(" sw:{}{}", if ui.swap_bytes {"B"} else {""}, if ui.swap_words {"W"} else {""})
                                } else {
                                    String::new()
                                };
                                t!("run_ui.status_tcp", func = &fi.func_name, base = format!("{:?}", ui.base), fmt = fmt, sw = sw)
                                } else {
                                    let fmt = ui.reg_format.short_label();
                                    let sw = if ui.swap_bytes || ui.swap_words {
                                        format!(" sw:{}{}", if ui.swap_bytes {"B"} else {""}, if ui.swap_words {"W"} else {""})
                                    } else {
                                        String::new()
                                    };
                                    t!("run_ui.status_rtu", func = &fi.func_name, base = format!("{:?}", ui.base), fmt = fmt, sw = sw)
                                }
                            } else {
                                let fmt = ui.reg_format.short_label();
                                let sw = if ui.swap_bytes || ui.swap_words {
                                    format!(" sw:{}{}", if ui.swap_bytes {"B"} else {""}, if ui.swap_words {"W"} else {""})
                                } else {
                                    String::new()
                                };
                                t!("run_ui.status_waiting", base = format!("{:?}", ui.base), fmt = fmt, sw = sw)
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
                                        let v = format_u16(val.unwrap(), ui.base);
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
                                                match parse_u16_str(&ui.edit_buf, ui.base) {
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
                                            if ui.edit_is_label || ui.edit_is_profile || edit_accepts_char(&ui.edit_buf, ch, ui.base) {
                                                ui.edit_buf.push(ch);
                                            } else {
                                                set_status(
                                                    &mut ui,
                                                    t!("run_ui.char_rejected"),
                                                );
                                            }
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
        args.main_mode = "tcp-monitor".to_string();
                                                            if let Err(e) = crate::modbus::run_modbus_monitor_tcp(args, mon_state).await {
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
                                                ui.selected = ui.selected.saturating_sub(1);
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
                                                let len = state.read().await.holding.len();
                                                ui.selected = (ui.selected + 1).min(len.saturating_sub(1));
                                            }
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

                                        // f: 循环切换当前寄存器的组合格式 (u16 → i32 → u64 → i128 → u16)
                                        KeyCode::Char('f') => {
                                            if !is_monitor_mode && !ui.edit_mode {
                                                let addr = ui.selected;
                                                let mut s = state.write().await;
                                                let combinations = match ui.reg_view {
                                                    REG_VIEW_HOLDING => &mut s.holding_combinations,
                                                    REG_VIEW_INPUT => &mut s.input_combinations,
                                                    _ => &mut s.holding_combinations,
                                                };
                                                let cur_fmt = combinations.get(&addr).copied().unwrap_or(RegDataFormat::U16);
                                                let next_fmt = cur_fmt.next_combination();
                                                if next_fmt == RegDataFormat::U16 {
                                                    combinations.remove(&addr);
                                                    drop(s);
                                                    set_status(&mut ui, t!("run_ui.combination_removed", addr = addr));
                                                } else {
                                                    combinations.insert(addr, next_fmt);
                                                    drop(s);
                                                    set_status(&mut ui, t!("run_ui.combination_set", addr = addr, fmt = next_fmt.short_label(), count = next_fmt.regs_needed()));
                                                }
                                            }
                                        }
                                        // F (Shift+F): 循环切换当前寄存器的组合格式（向后: u16 → i128 → u64 → i32 → u16）
                                        KeyCode::Char('F') => {
                                            if !is_monitor_mode && !ui.edit_mode {
                                                let addr = ui.selected;
                                                let mut s = state.write().await;
                                                let combinations = match ui.reg_view {
                                                    REG_VIEW_HOLDING => &mut s.holding_combinations,
                                                    REG_VIEW_INPUT => &mut s.input_combinations,
                                                    _ => &mut s.holding_combinations,
                                                };
                                                let cur_fmt = combinations.get(&addr).copied().unwrap_or(RegDataFormat::U16);
                                                // Reverse cycle: u16 → i128, i128 → u64, u64 → i32, i32 → u16
                                                let prev_fmt = match cur_fmt {
                                                    RegDataFormat::U16 => RegDataFormat::I128,
                                                    RegDataFormat::I32 => RegDataFormat::U16,
                                                    RegDataFormat::U64 => RegDataFormat::I32,
                                                    RegDataFormat::I128 => RegDataFormat::U64,
                                                    _ => RegDataFormat::I128,
                                                };
                                                if prev_fmt == RegDataFormat::U16 {
                                                    combinations.remove(&addr);
                                                    drop(s);
                                                    set_status(&mut ui, t!("run_ui.combination_removed", addr = addr));
                                                } else {
                                                    combinations.insert(addr, prev_fmt);
                                                    drop(s);
                                                    set_status(&mut ui, t!("run_ui.combination_set", addr = addr, fmt = prev_fmt.short_label(), count = prev_fmt.regs_needed()));
                                                }
                                            }
                                        }
                                        // w: 切换字节序交换
                                        KeyCode::Char('w') => {
                                            if !is_monitor_mode && !ui.edit_mode {
                                                let mut s = state.write().await;
                                                s.swap_bytes = !s.swap_bytes;
                                                ui.swap_bytes = s.swap_bytes;
                                                drop(s);
                                                if ui.swap_bytes { set_status(&mut ui, "Byte swap: ON"); }
                                                else { set_status(&mut ui, "Byte swap: OFF"); }
                                            }
                                        }
                                        // W (Shift+W): 切换字序交换
                                        KeyCode::Char('W') => {
                                            if !is_monitor_mode && !ui.edit_mode {
                                                let mut s = state.write().await;
                                                s.swap_words = !s.swap_words;
                                                ui.swap_words = s.swap_words;
                                                drop(s);
                                                if ui.swap_words { set_status(&mut ui, "Word swap: ON"); }
                                                else { set_status(&mut ui, "Word swap: OFF"); }
                                            }
                                        }
                                        // E (Shift+E): 导出当前寄存器到 JSON 文件
                                        KeyCode::Char('E') => {
                                            if !is_monitor_mode && !ui.edit_mode {
                                                let s = state.read().await;
                                                match export_registers_to_json(ui.reg_format, ui.swap_bytes, ui.swap_words, ui.base, &s) {
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
                                            if ui.selected < s.holding.len() {
                                                ui.edit_mode = true;
                                                ui.edit_is_label = false;
                                                ui.edit_is_profile = false;
                                                ui.edit_buf = format_u16(s.holding[ui.selected], ui.base);
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
                                        // G: 跳转寄存器地址
                                        KeyCode::Char('G') => {
                                            if !is_monitor_mode && !ui.edit_mode && !ui.show_monitor {
                                                ui.goto_mode = true;
                                                ui.goto_buf.clear();
                                                ui.status_msg = Some("Go to address: ".to_string());
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

/// Check if a register address is a secondary (non-primary) register in a combination
fn is_secondary_register(addr: usize, combinations: &HashMap<usize, crate::RegDataFormat>) -> bool {
    for (&primary_addr, &fmt) in combinations {
        let count = fmt.regs_needed();
        if addr > primary_addr && addr < primary_addr + count {
            return true;
        }
    }
    false
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

    // 搜索过滤：构建匹配索引列表
    let filtered_indices: Vec<usize> = if ui.search_buf.is_empty() {
        (0..len).collect()
    } else {
        let search_lower = ui.search_buf.to_lowercase();
        items
            .iter()
            .enumerate()
            .filter(|(i, _v)| {
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

    // 调整 selected 到过滤列表中
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
        .filter_map(|&i| {
            let v = &items[i];
            if is_bool {
                let val = if *v == 1 { "ON" } else { "OFF" };
                Some(Row::new(vec![Cell::from(format!("{}", i)), Cell::from(val)]))
            } else {
                // Check if this register is a secondary (disabled) register in a combination
                let combinations = match ui.reg_view {
                    REG_VIEW_HOLDING => &s.holding_combinations,
                    REG_VIEW_INPUT => &s.input_combinations,
                    _ => &s.holding_combinations,
                };
                if is_secondary_register(i, combinations) {
                    return None; // Skip secondary registers
                }

                // Determine the format for this register
                let reg_fmt = combinations.get(&i).copied().unwrap_or(ui.reg_format);
                let mut val = format_register_value(items, i, reg_fmt, ui.base, ui.swap_bytes, ui.swap_words);
                let mut label = labels.and_then(|l| l.get(i).cloned()).unwrap_or_default();
                // Show combination info in label if combined
                if let Some(&combo_fmt) = combinations.get(&i) {
                    let combo_label = format!("[{}×{}]", combo_fmt.regs_needed(), combo_fmt.short_label());
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
                    Some(Row::new(vec![
                        Cell::from(format!("{}", i)),
                        Cell::from(label),
                        Cell::from(val),
                        Cell::from(change_str),
                        Cell::from(Line::from(bar_spans)),
                    ]))
                } else {
                    Some(Row::new(vec![
                        Cell::from(format!("{}", i)),
                        Cell::from(label),
                        Cell::from(val),
                        Cell::from(change_str),
                    ]))
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

/// 渲染值变化历史条形图
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

/// 构建字节流面板的显示文本
fn format_byte_panel(fi: &FrameInfo) -> String {
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

/// 格式化监听历史流水
fn format_monitor_history(m: &MonitorStats, scroll: usize) -> String {
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

/// 生成协议分析文本（字节级分析 + CRC 校验）
fn format_protocol_analysis(rec: &FrameRecord) -> String {
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

/// 格式化监听统计一览
fn format_monitor_stats(m: &MonitorStats) -> String {
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

/// 估算文本在指定宽度下需要多少行（简单字符计数，支持 \n 换行）
fn wrapped_lines(text: &str, width: usize) -> usize {
    if width == 0 {
        return 0;
    }
    let mut lines = 1;
    let mut col = 0;
    for ch in text.chars() {
        if ch == '\n' {
            lines += 1;
            col = 0;
        } else {
            // 中文/全角字符占 2 列，其他占 1 列
            let w = if ch as u32 > 0x7F { 2 } else { 1 };
            if col + w > width {
                lines += 1;
                col = w;
            } else {
                col += w;
            }
        }
    }
    lines
}

/// 将 RegChangePattern 转换为模式列表索引
fn pattern_index(p: &crate::RegChangePattern) -> usize {
    match p {
        crate::RegChangePattern::Random => 0,
        crate::RegChangePattern::UpDown => 1,
        crate::RegChangePattern::Sine => 2,
        crate::RegChangePattern::Square => 3,
        crate::RegChangePattern::Triangle => 4,
    }
}

/// 将模式列表索引转换为 RegChangePattern
fn index_to_pattern(idx: usize) -> crate::RegChangePattern {
    match idx {
        0 => crate::RegChangePattern::Random,
        1 => crate::RegChangePattern::UpDown,
        2 => crate::RegChangePattern::Sine,
        3 => crate::RegChangePattern::Square,
        _ => crate::RegChangePattern::Triangle,
    }
}

/// 将对话框中的模式选择写入 AppState
fn apply_pattern_dialog(ui: &mut Ui, s: &mut crate::AppState) {
    let pattern = index_to_pattern(ui.pattern_dialog_sel);
    let addr = ui.pattern_dialog_addr;
    match ui.pattern_dialog_reg_type {
        REG_VIEW_HOLDING => {
            while s.holding_change_patterns.len() <= addr {
                s.holding_change_patterns
                    .push(crate::RegChangePattern::Random);
                s.holding_pattern_freqs.push(1.0);
            }
            s.holding_change_patterns[addr] = pattern;
            s.holding_pattern_freqs[addr] = ui.pattern_dialog_freq;
        }
        REG_VIEW_INPUT => {
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

/// 渲染寄存器变化模式配置对话框
fn render_pattern_dialog(f: &mut ratatui::Frame<'_>, ui: &Ui, s: &crate::AppState) {
    let patterns = ["Random", "Up/Down", "Sine", "Square", "Triangle"];
    let addr = ui.pattern_dialog_addr;

    let current_val = match ui.pattern_dialog_reg_type {
        REG_VIEW_HOLDING if addr < s.holding.len() => format_u16(s.holding[addr], ui.base),
        REG_VIEW_INPUT if addr < s.input_registers.len() => {
            format_u16(s.input_registers[addr], ui.base)
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

fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::vertical([
        Constraint::Length((r.height * percent_y / 200).saturating_sub(1)),
        Constraint::Length(r.height * percent_y / 100),
        Constraint::Length((r.height * percent_y / 200).saturating_sub(1)),
    ])
    .split(r);

    Layout::horizontal([
        Constraint::Length((r.width * percent_x / 200).saturating_sub(1)),
        Constraint::Length(r.width * percent_x / 100),
        Constraint::Length((r.width * percent_x / 200).saturating_sub(1)),
    ])
    .split(popup_layout[1])[1]
}
