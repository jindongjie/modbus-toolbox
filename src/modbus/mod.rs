pub mod frame;
pub mod service;
pub mod client;
pub mod server;
pub mod worker;
pub mod monitor;

#[cfg(test)]
mod tests;

use std::time::Duration;
use tokio::sync::oneshot;
use tokio_modbus::prelude::*;

pub(crate) const IO_TIMEOUT: Duration = Duration::from_secs(5);

pub enum RegCmd {
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
    ReadCoils {
        addr: usize,
        cnt: usize,
        resp: oneshot::Sender<std::result::Result<Vec<bool>, ExceptionCode>>,
    },
    WriteSingleCoil {
        addr: usize,
        value: bool,
        resp: oneshot::Sender<std::result::Result<(), ExceptionCode>>,
    },
    WriteMultipleCoils {
        addr: usize,
        values: Vec<bool>,
        resp: oneshot::Sender<std::result::Result<(), ExceptionCode>>,
    },
    ReadDiscreteInputs {
        addr: usize,
        cnt: usize,
        resp: oneshot::Sender<std::result::Result<Vec<bool>, ExceptionCode>>,
    },
    ReadInputRegisters {
        addr: usize,
        cnt: usize,
        resp: oneshot::Sender<std::result::Result<Vec<u16>, ExceptionCode>>,
    },
    /// 从设备扫描：遍历 1..255，读保持寄存器 0，返回 (slave_id, Option<value>)
    SlaveScan {
        resp: oneshot::Sender<Vec<(u8, Option<u16>)>>,
    },
}

// Re-export every submodule's public items at the modbus level so `modbus::X`,
// `use modbus::*;` (main.rs) and `crate::modbus::{calc_crc16, frame_bytes_from_info,
// run_modbus_monitor_*}` (ui/*) keep working exactly as with the original flat module.
pub use client::*;
pub use server::*;
pub use worker::*;
pub use monitor::*;
pub use frame::*;
