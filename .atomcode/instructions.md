# AtomCode 技能 — Modbus 工具箱

## 项目概述

跨平台 Modbus 通讯调试工具（TUI），单文件零依赖发布。

## 技术栈

| 层 | 技术 |
|---|------|
| 运行时 | `tokio`（多线程） |
| Modbus 协议 | `tokio-modbus`（RTU/TCP 服务端+客户端） |
| 串口 | `tokio-serial` |
| 终端 UI | `ratatui` + `crossterm` |
| 配置解析 | `clap`（CLI）+ `toml` + `serde`（配置文件） |
| 错误处理 | `anyhow` |
| 异步 | `futures` |

## 核心文件职责

| 文件 | 职责 |
|------|------|
| `src/main.rs` | 入口、参数解析、状态初始化、任务编排 |
| `src/modbus.rs` | Modbus 协议实现（HoldingService、4 种模式的 run_* 函数、client_read_write_loop） |
| `src/ui.rs` | ratatui TUI 渲染 + 键盘事件处理（run_ui） |
| `config.toml` | 持久化配置（Toml 格式，多 profile） |

## 数据流

```
CLI Args / config.toml  ──>  AppState (Arc<RwLock>)  <── reg_worker_loop <──RegCmd──> Modbus 任务
                                    ^
                                    │ 共享引用
                                    v
                                  UI (ratatui)
```

- 寄存器访问通过 `mpsc::unbounded_channel<RegCmd>` 发送命令
- `reg_worker_loop` 仲裁所有读写请求，保证 `AppState` 的线程安全
- UI 和 Modbus 任务通过 `Arc<RwLock<AppState>>` 共享读取

## 4 种运行模式

| 模式 | CLI 短名 | 函数 |
|------|----------|------|
| TCP 服务端 | `ts` | `run_modbus_tcp_server` |
| TCP 客户端 | `tc` | `run_modbus_tcp_client` |
| RTU 服务端 | `rs` | `run_modbus_rtu_server` |
| RTU 客户端 | `rc` | `run_modbus_rtu_client` |

## 代码规范

- **命名**: snake_case（变量/函数）、PascalCase（类型/trait）
- **错误处理**: 所有函数返回 `Result<()>`（anyhow），不用 `unwrap()`，用 `.context("描述")`
- **导入顺序**: 标准库 → 第三方 crate → 本地模块，每组空一行
- **UI**: 编辑模式（`edit_mode: true`）下键盘输入进 `edit_buf`；非编辑模式处理导航和操作快捷键
- **通讯风格**: 我是中文开发者，直接用中文交流即可
- **修改验证**: 每次修改后运行 `cargo check` 确认无编译错误
- **行号引用**: 给出修改建议时带文件名和行号

## 常用命令

```bash
cargo check                    # 快速验证编译
cargo run -- --profile tc      # TCP 客户端模式
cargo run -- -m ts -p 502      # TCP 服务端模式
cargo run -- -m rc -d /dev/ttyUSB0 -b 9600  # RTU 客户端
cargo clippy                   # 代码检查
```

## 已知问题

1. **串口错误卡死**: 串口出错后 `set_status` 更新状态栏会导致界面卡死（未解决）
2. **tokio_unstable**: `.cargo/config.toml` 需要 `--cfg tokio_unstable` 标志

## 跨文件约束

- 改 `modbus.rs` 的 run_* 函数签名 → 同时检查 `main.rs` 的 spawn 调用
- 改 `ui.rs` 的 `Ui` struct → 同步更新 `Ui::new()` 和 `render()`
- 新增 CLI 参数 → 同时更新 `Args` struct、`Default` impl、`config.toml` 示例
- 修改 `AppState` → 检查所有 `Arc<RwLock<AppState>>` 的使用点

# 版本管理
- 每次修改用git提交
