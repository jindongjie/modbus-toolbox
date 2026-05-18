# Modbus Toolbox

<img src="art/logo.png" alt="logo" width="300" height="300">  

Modbus Toolbox is a collection of tools created to help developers debug the Modbus communication protocol more quickly and stably. It implements both server and client for the RTU/TCP transport layers, and additionally features silent monitoring of RTU communications. The interface is presented through a terminal-based graphical interface (TUI) — streamlined, efficient, and runnable on all major operating systems via the command line. Single binary, zero runtime dependencies, an indispensable tool for debugging Modbus communications.

## Key Features

<img src="art/breif.png" alt="features overview" width="300" height="300">  

1. Modbus TCP/RTU Server & Client
   - TUI table interface, single/batch read and modify
   - Multiple data radix display and editing: decimal, binary, hexadecimal
   - Timed reads and writes for stability testing

2. Device Memory
   - Save discovered device connection parameters via configuration files; frequently used devices can be bookmarked
   - Annotate register addresses and create virtual variables for easier understanding

3. Modbus RTU Silent Monitoring
   - Real-time and historical flow logs
   - Statistics for read addresses, function codes, and frequency, presented in a summary table

## Compatibility (detailed list below)

<img src="art/comp.png" alt="compatibility" width="300" height="300">  

### Instruction Sets

1. I386
2. X86_64
3. Arm
4. RISC-V
5. MIPS

### Operating Systems

1. Windows 7 (requires VxKex API extension below 10) and above
2. Linux — no version restrictions
3. MacOS — no version restrictions
4. Other UNIX operating systems

## Usage Guide

'q': Quit  
'j,k': Move up/down through the register list  
For other keybindings, check the in-app help.

## Configuration

Settings are persisted via `config.toml`. You can customize configuration names and store multiple profiles.

## Development Guide

Know Rust and Tokio.

### TODO

- [ ] After a serial port error triggers `ui.status_msg` update, the UI freezes — not yet resolved.

## Dependencies

See `Cargo.toml` for details.

1. **ratatui** — terminal UI framework
2. **tokio** — async runtime for networking, serial, and Modbus
3. **anyhow** — error handling

## Compatibility Matrix

| Target Architecture             | GCC Ver | Clang Ver | C++  | Rust Ver | Status |
|------------------------------------------|----------|-------------|--------|------------|--------|
| aarch64-linux-android [1]                | 9.0.8    | 9.0.8       | ✓      | 6.1.0      | ✓      |
| aarch64-unknown-linux-gnu                | 2.23     | 5.4.0       | ✓      | 5.1.0      | ✓      |
| aarch64-unknown-linux-musl               | 1.1.24   | 9.2.0       | ✓      | 6.1.0      | ✓      |
| arm-linux-androideabi [1]                | 9.0.8    | 9.0.8       | ✓      | 6.1.0      | ✓      |
| arm-unknown-linux-gnueabi                | 2.23     | 5.4.0       | ✓      | 5.1.0      | ✓      |
| arm-unknown-linux-gnueabihf              | 2.17     | 8.3.0       | ✓      | 6.1.0      | ✓      |
| arm-unknown-linux-musleabi               | 1.1.24   | 9.2.0       | ✓      | 6.1.0      | ✓      |
| arm-unknown-linux-musleabihf             | 1.1.24   | 9.2.0       | ✓      | 6.1.0      | ✓      |
| armv5te-unknown-linux-gnueabi            | 2.27     | 7.5.0       | ✓      | 6.1.0      | ✓      |
| armv5te-unknown-linux-musleabi           | 1.1.24   | 9.2.0       | ✓      | 6.1.0      | ✓      |
| armv7-linux-androideabi [1]              | 9.0.8    | 9.0.8       | ✓      | 6.1.0      | ✓      |
| armv7-unknown-linux-gnueabi              | 2.27     | 7.5.0       | ✓      | 6.1.0      | ✓      |
| armv7-unknown-linux-gnueabihf            | 2.23     | 5.4.0       | ✓      | 5.1.0      | ✓      |
| armv7-unknown-linux-musleabi             | 1.1.24   | 9.2.0       | ✓      | 6.1.0      | ✓      |
| armv7-unknown-linux-musleabihf           | 1.1.24   | 9.2.0       | ✓      | 6.1.0      | ✓      |
| i586-unknown-linux-gnu                   | 2.23     | 5.4.0       | ✓      | N/A        | ✓      |
| i586-unknown-linux-musl                  | 1.1.24   | 9.2.0       | ✓      | N/A        | ✓      |
| i686-unknown-freebsd                     | 1.5      | 6.4.0       | ✓      | N/A        |        |
| i686-linux-android [1]                   | 9.0.8    | 9.0.8       | ✓      | 6.1.0      | ✓      |
| i686-pc-windows-gnu                      | N/A      | 7.5         | ✓      | N/A        | ✓      |
| i686-unknown-linux-gnu                   | 2.23     | 5.4.0       | ✓      | 5.1.0      | ✓      |
| i686-unknown-linux-musl                  | 1.1.24   | 9.2.0       | ✓      | N/A        | ✓      |
| mips-unknown-linux-gnu                   | 2.23     | 5.4.0       | ✓      | 5.1.0      | ✓      |
| mips-unknown-linux-musl                  | 1.1.24   | 9.2.0       | ✓      | 6.1.0      | ✓      |
| mips64-unknown-linux-gnuabi64            | 2.23     | 5.4.0       | ✓      | 5.1.0      | ✓      |
| mips64-unknown-linux-muslabi64           | 1.1.24   | 9.2.0       | ✓      | 6.1.0      | ✓      |
| mips64el-unknown-linux-gnuabi64          | 2.23     | 5.4.0       | ✓      | 5.1.0      | ✓      |
| mips64el-unknown-linux-muslabi64         | 1.1.24   | 9.2.0       | ✓      | 6.1.0      | ✓      |
| mipsel-unknown-linux-gnu                 | 2.23     | 5.4.0       | ✓      | 5.1.0      | ✓      |
| mipsel-unknown-linux-musl                | 1.1.24   | 9.2.0       | ✓      | 6.1.0      | ✓      |
| powerpc-unknown-linux-gnu                | 2.23     | 5.4.0       | ✓      | 5.1.0      | ✓      |
| powerpc64-unknown-linux-gnu              | 2.23     | 5.4.0       | ✓      | 5.1.0      | ✓      |
| powerpc64le-unknown-linux-gnu            | 2.23     | 5.4.0       | ✓      | 5.1.0      | ✓      |
| riscv64gc-unknown-linux-gnu              | 2.27     | 7.5.0       | ✓      | 6.1.0      | ✓      |
| s390x-unknown-linux-gnu                  | 2.23     | 5.4.0       | ✓      | 5.1.0      | ✓      |
| sparc64-unknown-linux-gnu                | 2.23     | 5.4.0       | ✓      | 5.1.0      | ✓      |
| sparcv9-sun-solaris                      | 1.22.7   | 8.4.0       | ✓      | N/A        |        |
| thumbv6m-none-eabi [4]                   | 2.2.0    | 4.9.3       |        | N/A        |        |
| thumbv7em-none-eabi [4]                  | 2.2.0    | 4.9.3       |        | N/A        |        |
| thumbv7em-none-eabihf [4]                | 2.2.0    | 4.9.3       |        | N/A        |        |
| thumbv7m-none-eabi [4]                   | 2.2.0    | 4.9.3       |        | N/A        |        |
| thumbv7neon-linux-androideabi [1]        | 9.0.8    | 9.0.8       | ✓      | 6.1.0      | ✓      |
| thumbv7neon-unknown-linux-gnueabihf      | 2.23     | 5.4.0       | ✓      | 5.1.0      | ✓      |
| wasm32-unknown-emscripten [6]            | 3.1.14   | 15.0.0      | ✓      | N/A        | ✓      |
| x86_64-linux-android [1]                 | 9.0.8    | 9.0.8       | ✓      | 6.1.0      | ✓      |
| x86_64-pc-windows-gnu                    | N/A      | 7.3         | ✓      | N/A        | ✓      |
| x86_64-sun-solaris                       | 1.22.7   | 8.4.0       | ✓      | N/A        |        |
| x86_64-unknown-freebsd                   | 1.5      | 6.4.0       | ✓      | N/A        |        |
| x86_64-unknown-dragonfly [2] [3]         | 6.0.1    | 5.3.0       | ✓      | N/A        |        |
| x86_64-unknown-illumos                   | 1.20.4   | 8.4.0       | ✓      | N/A        |        |
| x86_64-unknown-linux-gnu                 | 2.23     | 5.4.0       | ✓      | 5.1.0      | ✓      |
| x86_64-unknown-linux-gnu:centos [5]      | 2.17     | 4.8.5       | ✓      | 4.2.1      | ✓      |
| x86_64-unknown-linux-musl                | 1.1.24   | 9.2.0       | ✓      | N/A        | ✓      |
| x86_64-unknown-netbsd [3]                | 9.2.0    | 9.4.0       | ✓      | N/A        |        |

Notes:

1. Columns include target architecture, GCC version, Clang version, C++ support status, Rust version, and overall status.
2. "✓" indicates support or availability, "N/A" means not applicable or not provided.
3. Markers [1], [2], [3], [4], [5], [6] indicate notes for specific platforms or configurations.
