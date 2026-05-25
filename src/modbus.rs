use crate::Args;
use anyhow::{anyhow, Context, Result};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot, RwLock};
use tokio::time::timeout;
use tokio_modbus::server;
use tokio_modbus::server::tcp::accept_tcp_connection;
use tokio_serial::SerialPortBuilderExt;

use crate::parse_databits;
use crate::parse_flow;
use crate::parse_parity;
use crate::parse_stopbits;
use crate::record_frame;
use crate::AppState;
use crate::FrameInfo;
use tokio_modbus::prelude::*;

const IO_TIMEOUT: Duration = Duration::from_secs(5);

// ---------- CRC16 Modbus 计算 ----------
pub(crate) fn calc_crc16(data: &[u8]) -> u16 {
    let mut crc: u16 = 0xFFFF;
    for &byte in data {
        crc ^= byte as u16;
        for _ in 0..8 {
            if crc & 0x0001 != 0 {
                crc = (crc >> 1) ^ 0xA001;
            } else {
                crc >>= 1;
            }
        }
    }
    crc
}

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

//寄存器指令执行循环
pub async fn reg_worker_loop(
    state: Arc<RwLock<AppState>>,
    mut rx: mpsc::UnboundedReceiver<RegCmd>,
) {
    while let Some(cmd) = rx.recv().await {
        match cmd {
            RegCmd::ReadHolding { addr, cnt, resp } => {
                let out = {
                    let s = state.read().await;
                    if addr + cnt > s.holding.len() {
                        Err(ExceptionCode::IllegalDataAddress)
                    } else {
                        Ok(s.holding[addr..addr + cnt].to_vec())
                    }
                };
                let _ = resp.send(out);
            }
            RegCmd::WriteSingleHolding { addr, value, resp } => {
                let out = {
                    let mut s = state.write().await;
                    if addr >= s.holding.len() {
                        Err(ExceptionCode::IllegalDataAddress)
                    } else {
                        let old = s.holding[addr];
                        s.holding[addr] = value;
                        if old != value {
                            crate::record_reg_change(&mut s, addr, old, value);
                        }
                        Ok(())
                    }
                };
                let _ = resp.send(out);
            }
            RegCmd::WriteMultipleHolding { addr, values, resp } => {
                let out = {
                    let mut s = state.write().await;
                    let len = s.holding.len();
                    if addr + values.len() > len {
                        Err(ExceptionCode::IllegalDataAddress)
                    } else {
                        for (i, &v) in values.iter().enumerate() {
                            let idx = addr + i;
                            let old = s.holding[idx];
                            s.holding[idx] = v;
                            if old != v {
                                crate::record_reg_change(&mut s, idx, old, v);
                            }
                        }
                        Ok(())
                    }
                };
                let _ = resp.send(out);
            }
            RegCmd::ReadCoils { addr, cnt, resp } => {
                let out = {
                    let s = state.read().await;
                    if addr + cnt > s.coils.len() {
                        Err(ExceptionCode::IllegalDataAddress)
                    } else {
                        Ok(s.coils[addr..addr + cnt].to_vec())
                    }
                };
                let _ = resp.send(out);
            }
            RegCmd::WriteSingleCoil { addr, value, resp } => {
                let out = {
                    let mut s = state.write().await;
                    if addr >= s.coils.len() {
                        Err(ExceptionCode::IllegalDataAddress)
                    } else {
                        let old = s.coils[addr];
                        s.coils[addr] = value;
                        if old != value {
                            crate::record_reg_change(
                                &mut s,
                                addr,
                                if old { 1u16 } else { 0u16 },
                                if value { 1u16 } else { 0u16 },
                            );
                        }
                        Ok(())
                    }
                };
                let _ = resp.send(out);
            }
            RegCmd::WriteMultipleCoils { addr, values, resp } => {
                let out = {
                    let mut s = state.write().await;
                    let len = s.coils.len();
                    if addr + values.len() > len {
                        Err(ExceptionCode::IllegalDataAddress)
                    } else {
                        for (i, &v) in values.iter().enumerate() {
                            let idx = addr + i;
                            let old = s.coils[idx];
                            s.coils[idx] = v;
                            if old != v {
                                crate::record_reg_change(
                                    &mut s,
                                    idx,
                                    if old { 1u16 } else { 0u16 },
                                    if v { 1u16 } else { 0u16 },
                                );
                            }
                        }
                        Ok(())
                    }
                };
                let _ = resp.send(out);
            }
            RegCmd::ReadDiscreteInputs { addr, cnt, resp } => {
                let out = {
                    let s = state.read().await;
                    if addr + cnt > s.discrete.len() {
                        Err(ExceptionCode::IllegalDataAddress)
                    } else {
                        Ok(s.discrete[addr..addr + cnt].to_vec())
                    }
                };
                let _ = resp.send(out);
            }
            RegCmd::ReadInputRegisters { addr, cnt, resp } => {
                let out = {
                    let s = state.read().await;
                    if addr + cnt > s.input_registers.len() {
                        Err(ExceptionCode::IllegalDataAddress)
                    } else {
                        Ok(s.input_registers[addr..addr + cnt].to_vec())
                    }
                };
                let _ = resp.send(out);
            }
            RegCmd::SlaveScan { resp } => {
                // 服务端模式下不支持扫描，返回空结果
                let _ = resp.send(Vec::new());
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
    state: Arc<RwLock<AppState>>,
    is_tcp: bool,
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

        let is_tcp = self.is_tcp;

        match req.request {
            Request::ReadHoldingRegisters(addr, cnt) => {
                let addr = addr as usize;
                let cnt = cnt as usize;
                let holding_len = self.holding_len;
                let tx: mpsc::UnboundedSender<RegCmd> = self.tx.clone();
                let state = Arc::clone(&self.state);
                let unit = self.unit;

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
                        Ok(Ok(values)) => {
                            let fi = FrameInfo {
                                is_tcp,
                                unit,
                                func_code: 0x03,
                                func_name: format!("读保持寄存器"),
                                addr: addr as u16,
                                values: values.clone(),
                                is_request: false,
                            };
                            record_frame(&mut state.write().await.monitor, &fi);
                            state.write().await.last_frame = Some(fi);
                            Ok(Response::ReadHoldingRegisters(values))
                        }
                        Ok(Err(ex)) => Err(ex),
                        Err(_closed) => Err(ExceptionCode::ServerDeviceFailure),
                    }
                })
            }

            Request::WriteSingleRegister(addr, value) => {
                let addr = addr as usize;
                let holding_len = self.holding_len;
                let tx = self.tx.clone();
                let state = Arc::clone(&self.state);
                let unit = self.unit;

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
                        Ok(Ok(())) => {
                            let fi = FrameInfo {
                                is_tcp,
                                unit,
                                func_code: 0x06,
                                func_name: format!("写单寄存器"),
                                addr: addr as u16,
                                values: vec![value],
                                is_request: false,
                            };
                            record_frame(&mut state.write().await.monitor, &fi);
                            state.write().await.last_frame = Some(fi);
                            Ok(Response::WriteSingleRegister(addr as u16, value))
                        }
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
                let state = Arc::clone(&self.state);
                let unit = self.unit;

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
                        Ok(Ok(())) => {
                            let fi = FrameInfo {
                                is_tcp,
                                unit,
                                func_code: 0x10,
                                func_name: format!("写多寄存器"),
                                addr: addr as u16,
                                values: values.to_vec(),
                                is_request: false,
                            };
                            record_frame(&mut state.write().await.monitor, &fi);
                            state.write().await.last_frame = Some(fi);
                            Ok(Response::WriteMultipleRegisters(addr, qty))
                        }
                        Ok(Err(ex)) => Err(ex),
                        Err(_closed) => Err(ExceptionCode::ServerDeviceFailure),
                    }
                })
            }

            Request::ReadCoils(addr, cnt) => {
                let addr = addr as usize;
                let cnt = cnt as usize;
                let tx = self.tx.clone();
                let state = Arc::clone(&self.state);
                let unit = self.unit;

                boxed(async move {
                    let max = state.read().await.coils.len();
                    if addr + cnt > max {
                        return Err(ExceptionCode::IllegalDataAddress);
                    }

                    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
                    if tx
                        .send(RegCmd::ReadCoils {
                            addr,
                            cnt,
                            resp: resp_tx,
                        })
                        .is_err()
                    {
                        return Err(ExceptionCode::ServerDeviceFailure);
                    }

                    match resp_rx.await {
                        Ok(Ok(values)) => {
                            let fi = FrameInfo {
                                is_tcp,
                                unit,
                                func_code: 0x01,
                                func_name: format!("读线圈"),
                                addr: addr as u16,
                                values: values
                                    .iter()
                                    .map(|&b| if b { 1u16 } else { 0u16 })
                                    .collect(),
                                is_request: false,
                            };
                            record_frame(&mut state.write().await.monitor, &fi);
                            state.write().await.last_frame = Some(fi);
                            Ok(Response::ReadCoils(values))
                        }
                        Ok(Err(ex)) => Err(ex),
                        Err(_closed) => Err(ExceptionCode::ServerDeviceFailure),
                    }
                })
            }

            Request::WriteSingleCoil(addr, value) => {
                let addr = addr as usize;
                let tx = self.tx.clone();
                let state = Arc::clone(&self.state);
                let unit = self.unit;

                boxed(async move {
                    let max = state.read().await.coils.len();
                    if addr >= max {
                        return Err(ExceptionCode::IllegalDataAddress);
                    }

                    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
                    if tx
                        .send(RegCmd::WriteSingleCoil {
                            addr,
                            value,
                            resp: resp_tx,
                        })
                        .is_err()
                    {
                        return Err(ExceptionCode::ServerDeviceFailure);
                    }

                    match resp_rx.await {
                        Ok(Ok(())) => {
                            let fi = FrameInfo {
                                is_tcp,
                                unit,
                                func_code: 0x05,
                                func_name: format!("写单线圈"),
                                addr: addr as u16,
                                values: vec![if value { 0xFF00u16 } else { 0x0000u16 }],
                                is_request: false,
                            };
                            record_frame(&mut state.write().await.monitor, &fi);
                            state.write().await.last_frame = Some(fi);
                            Ok(Response::WriteSingleCoil(addr as u16, value))
                        }
                        Ok(Err(ex)) => Err(ex),
                        Err(_closed) => Err(ExceptionCode::ServerDeviceFailure),
                    }
                })
            }

            Request::WriteMultipleCoils(addr, values) => {
                let addr = addr as usize;
                let values_vec = values.to_vec();
                let qty = values_vec.len() as u16;
                let tx = self.tx.clone();
                let state = Arc::clone(&self.state);
                let unit = self.unit;

                boxed(async move {
                    let max = state.read().await.coils.len();
                    if addr + values_vec.len() > max {
                        return Err(ExceptionCode::IllegalDataAddress);
                    }

                    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
                    if tx
                        .send(RegCmd::WriteMultipleCoils {
                            addr,
                            values: values_vec,
                            resp: resp_tx,
                        })
                        .is_err()
                    {
                        return Err(ExceptionCode::ServerDeviceFailure);
                    }

                    match resp_rx.await {
                        Ok(Ok(())) => {
                            let fi = FrameInfo {
                                is_tcp,
                                unit,
                                func_code: 0x0F,
                                func_name: format!("写多线圈"),
                                addr: addr as u16,
                                values: values
                                    .to_vec()
                                    .into_iter()
                                    .map(|b| if b { 1u16 } else { 0u16 })
                                    .collect(),
                                is_request: false,
                            };
                            record_frame(&mut state.write().await.monitor, &fi);
                            state.write().await.last_frame = Some(fi);
                            Ok(Response::WriteMultipleCoils(addr as u16, qty))
                        }
                        Ok(Err(ex)) => Err(ex),
                        Err(_closed) => Err(ExceptionCode::ServerDeviceFailure),
                    }
                })
            }

            Request::ReadDiscreteInputs(addr, cnt) => {
                let addr = addr as usize;
                let cnt = cnt as usize;
                let tx = self.tx.clone();
                let state = Arc::clone(&self.state);
                let unit = self.unit;

                boxed(async move {
                    let max = state.read().await.discrete.len();
                    if addr + cnt > max {
                        return Err(ExceptionCode::IllegalDataAddress);
                    }

                    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
                    if tx
                        .send(RegCmd::ReadDiscreteInputs {
                            addr,
                            cnt,
                            resp: resp_tx,
                        })
                        .is_err()
                    {
                        return Err(ExceptionCode::ServerDeviceFailure);
                    }

                    match resp_rx.await {
                        Ok(Ok(values)) => {
                            let fi = FrameInfo {
                                is_tcp,
                                unit,
                                func_code: 0x02,
                                func_name: format!("读离散输入"),
                                addr: addr as u16,
                                values: values
                                    .iter()
                                    .map(|&b| if b { 1u16 } else { 0u16 })
                                    .collect(),
                                is_request: false,
                            };
                            record_frame(&mut state.write().await.monitor, &fi);
                            state.write().await.last_frame = Some(fi);
                            Ok(Response::ReadDiscreteInputs(values))
                        }
                        Ok(Err(ex)) => Err(ex),
                        Err(_closed) => Err(ExceptionCode::ServerDeviceFailure),
                    }
                })
            }

            Request::ReadInputRegisters(addr, cnt) => {
                let addr = addr as usize;
                let cnt = cnt as usize;
                let tx = self.tx.clone();
                let state = Arc::clone(&self.state);
                let unit = self.unit;

                boxed(async move {
                    let max = state.read().await.input_registers.len();
                    if addr + cnt > max {
                        return Err(ExceptionCode::IllegalDataAddress);
                    }

                    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
                    if tx
                        .send(RegCmd::ReadInputRegisters {
                            addr,
                            cnt,
                            resp: resp_tx,
                        })
                        .is_err()
                    {
                        return Err(ExceptionCode::ServerDeviceFailure);
                    }

                    match resp_rx.await {
                        Ok(Ok(values)) => {
                            let fi = FrameInfo {
                                is_tcp,
                                unit,
                                func_code: 0x04,
                                func_name: format!("读输入寄存器"),
                                addr: addr as u16,
                                values: values.clone(),
                                is_request: false,
                            };
                            record_frame(&mut state.write().await.monitor, &fi);
                            state.write().await.last_frame = Some(fi);
                            Ok(Response::ReadInputRegisters(values))
                        }
                        Ok(Err(ex)) => Err(ex),
                        Err(_closed) => Err(ExceptionCode::ServerDeviceFailure),
                    }
                })
            }

            _ => boxed(async { Err(ExceptionCode::IllegalFunction) }),
        }
    }
}

pub async fn client_read_write_loop(
    mut ctx: tokio_modbus::client::Context,
    args: Args,
    state: Arc<RwLock<AppState>>,
    mut rx: mpsc::UnboundedReceiver<RegCmd>,
) -> Result<()> {
    let tick = Duration::from_millis(args.client_tick_ms);
    let mut interval = tokio::time::interval(tick);

    let once_max_reg_cnt: usize = 120;
    loop {
        interval.tick().await;

        //写入 (处理来自 UI 的待发往服务端的写命令)
        while let Ok(cmd) = rx.try_recv() {
            match cmd {
                RegCmd::WriteSingleHolding { addr, value, resp } => {
                    let out =
                        match timeout(IO_TIMEOUT, ctx.write_single_register(addr as u16, value))
                            .await
                        {
                            Ok(Ok(Ok(_))) => Ok(()),
                            Ok(Ok(Err(e))) => Err(e),
                            Ok(Err(_)) => Err(ExceptionCode::ServerDeviceFailure),
                            Err(_) => {
                                let _ = resp.send(Err(ExceptionCode::ServerDeviceFailure));
                                continue;
                            }
                        };
                    let _ = resp.send(out);
                }
                RegCmd::WriteMultipleHolding { addr, values, resp } => {
                    let mut start = addr;
                    let mut idx = 0usize;
                    let mut final_out: std::result::Result<(), ExceptionCode> = Ok(());

                    while idx < values.len() {
                        let chunk_len = (values.len() - idx).min(once_max_reg_cnt);
                        let chunk = &values[idx..idx + chunk_len];

                        let out = match timeout(
                            IO_TIMEOUT,
                            ctx.write_multiple_registers(start as u16, chunk),
                        )
                        .await
                        {
                            Ok(Ok(Ok(_))) => Ok(()),
                            Ok(Ok(Err(e))) => Err(e),
                            Ok(Err(_)) => Err(ExceptionCode::ServerDeviceFailure),
                            Err(_) => {
                                final_out = Err(ExceptionCode::ServerDeviceFailure);
                                break;
                            }
                        };

                        if out.is_err() {
                            final_out = out;
                            break;
                        }

                        start += chunk_len;
                        idx += chunk_len;
                    }

                    let _ = resp.send(final_out);
                }
                RegCmd::WriteSingleCoil { addr, value, resp } => {
                    let out = match timeout(IO_TIMEOUT, ctx.write_single_coil(addr as u16, value))
                        .await
                    {
                        Ok(Ok(Ok(_))) => Ok(()),
                        Ok(Ok(Err(e))) => Err(e),
                        Ok(Err(_)) => Err(ExceptionCode::ServerDeviceFailure),
                        Err(_) => {
                            let _ = resp.send(Err(ExceptionCode::ServerDeviceFailure));
                            continue;
                        }
                    };
                    let _ = resp.send(out);
                }
                RegCmd::WriteMultipleCoils { addr, values, resp } => {
                    let mut start = addr;
                    let mut idx = 0usize;
                    let mut final_out: std::result::Result<(), ExceptionCode> = Ok(());

                    while idx < values.len() {
                        let chunk_len = (values.len() - idx).min(once_max_reg_cnt);
                        let chunk = &values[idx..idx + chunk_len];

                        let out = match timeout(
                            IO_TIMEOUT,
                            ctx.write_multiple_coils(start as u16, chunk),
                        )
                        .await
                        {
                            Ok(Ok(Ok(_))) => Ok(()),
                            Ok(Ok(Err(e))) => Err(e),
                            Ok(Err(_)) => Err(ExceptionCode::ServerDeviceFailure),
                            Err(_) => {
                                final_out = Err(ExceptionCode::ServerDeviceFailure);
                                break;
                            }
                        };

                        if out.is_err() {
                            final_out = out;
                            break;
                        }

                        start += chunk_len;
                        idx += chunk_len;
                    }

                    let _ = resp.send(final_out);
                }
                RegCmd::SlaveScan { resp } => {
                    let mut results = Vec::new();
                    for id in 1..=255u8 {
                        ctx.set_slave(Slave(id));
                        match timeout(IO_TIMEOUT, ctx.read_holding_registers(0, 1)).await {
                            Ok(Ok(Ok(values))) => {
                                results.push((id, Some(values[0])));
                            }
                            _ => {
                                results.push((id, None));
                            }
                        }
                    }
                    let _ = resp.send(results);
                }
                _ => {}
            }
        }

        //读取保持寄存器
        if state.read().await.read_enabled[0] {
            let mut offset: usize = 0;

            while offset < args.holding_count {
                let cnt = (args.holding_count - offset).min(once_max_reg_cnt);
                let addr = offset as u16;

                match timeout(IO_TIMEOUT, ctx.read_holding_registers(addr, cnt as u16)).await {
                    Ok(Ok(rsp)) => match rsp {
                        Ok(values) => {
                            let mut s = state.write().await;
                            let end = (offset + values.len()).min(s.holding.len());
                            let write_len = end.saturating_sub(offset);
                            if write_len > 0 {
                                for i in 0..write_len {
                                    let idx = offset + i;
                                    let old = s.holding[idx];
                                    let new = values[i];
                                    if old != new {
                                        s.holding[idx] = new;
                                        crate::record_reg_change(&mut s, idx, old, new);
                                    }
                                }
                            }
                            if write_len > 0 {
                                let fi = FrameInfo {
                                    is_tcp: s.is_tcp,
                                    unit: args.unit,
                                    func_code: 0x03,
                                    func_name: t!("func_code.read_holding").to_string(),
                                    addr,
                                    values: values[..write_len].to_vec(),
                                    is_request: false,
                                };
                                record_frame(&mut s.monitor, &fi);
                                s.last_frame = Some(fi);
                            }
                            offset += cnt;
                        }
                        Err(e) => {
                            return Err(anyhow!(t!(
                                "modbus.exception_response",
                                err = format!("{:?}", e)
                            )));
                        }
                    },
                    Ok(Err(e)) => {
                        return Err(anyhow!(t!("modbus.client_read_fail", e = e)));
                    }
                    Err(_) => {
                        return Err(anyhow!(t!("modbus.client_read_timeout")));
                    }
                }
            }
        }

        //读取线圈
        if state.read().await.read_enabled[1] {
            let mut offset: usize = 0;
            while offset < args.coil_count {
                let cnt = (args.coil_count - offset).min(once_max_reg_cnt);
                let addr = offset as u16;

                match timeout(IO_TIMEOUT, ctx.read_coils(addr, cnt as u16)).await {
                    Ok(Ok(rsp)) => match rsp {
                        Ok(values) => {
                            let mut s = state.write().await;
                            let end = (offset + values.len()).min(s.coils.len());
                            let write_len = end.saturating_sub(offset);
                            if write_len > 0 {
                                for i in 0..write_len {
                                    let idx = offset + i;
                                    let old = s.coils[idx];
                                    let new = values[i];
                                    if old != new {
                                        s.coils[idx] = new;
                                        crate::record_reg_change(
                                            &mut s,
                                            idx,
                                            if old { 1u16 } else { 0u16 },
                                            if new { 1u16 } else { 0u16 },
                                        );
                                    }
                                }
                            }
                            if write_len > 0 {
                                let fi = FrameInfo {
                                    is_tcp: s.is_tcp,
                                    unit: args.unit,
                                    func_code: 0x01,
                                    func_name: t!("func_code.read_coils").to_string(),
                                    addr,
                                    values: values[..write_len]
                                        .iter()
                                        .map(|&b| if b { 1u16 } else { 0u16 })
                                        .collect(),
                                    is_request: false,
                                };
                                record_frame(&mut s.monitor, &fi);
                                s.last_frame = Some(fi);
                            }
                            offset += cnt;
                        }
                        Err(e) => {
                            return Err(anyhow!(t!(
                                "modbus.exception_response",
                                err = format!("{:?}", e)
                            )));
                        }
                    },
                    Ok(Err(e)) => {
                        return Err(anyhow!(t!("modbus.client_read_fail", e = e)));
                    }
                    Err(_) => {
                        return Err(anyhow!(t!("modbus.client_read_timeout")));
                    }
                }
            }
        }

        //读取离散输入
        if state.read().await.read_enabled[2] {
            let mut offset: usize = 0;
            while offset < args.discrete_count {
                let cnt = (args.discrete_count - offset).min(once_max_reg_cnt);
                let addr = offset as u16;

                match timeout(IO_TIMEOUT, ctx.read_discrete_inputs(addr, cnt as u16)).await {
                    Ok(Ok(rsp)) => match rsp {
                        Ok(values) => {
                            let mut s = state.write().await;
                            let end = (offset + values.len()).min(s.discrete.len());
                            let write_len = end.saturating_sub(offset);
                            if write_len > 0 {
                                for i in 0..write_len {
                                    let idx = offset + i;
                                    s.discrete[idx] = values[i];
                                }
                            }
                            if write_len > 0 {
                                let fi = FrameInfo {
                                    is_tcp: s.is_tcp,
                                    unit: args.unit,
                                    func_code: 0x02,
                                    func_name: t!("func_code.read_discrete").to_string(),
                                    addr,
                                    values: values[..write_len]
                                        .iter()
                                        .map(|&b| if b { 1u16 } else { 0u16 })
                                        .collect(),
                                    is_request: false,
                                };
                                record_frame(&mut s.monitor, &fi);
                                s.last_frame = Some(fi);
                            }
                            offset += cnt;
                        }
                        Err(e) => {
                            return Err(anyhow!(t!(
                                "modbus.exception_response",
                                err = format!("{:?}", e)
                            )));
                        }
                    },
                    Ok(Err(e)) => {
                        return Err(anyhow!(t!("modbus.client_read_fail", e = e)));
                    }
                    Err(_) => {
                        return Err(anyhow!(t!("modbus.client_read_timeout")));
                    }
                }
            }
        }

        //读取输入寄存器
        if state.read().await.read_enabled[3] {
            let mut offset: usize = 0;
            while offset < args.input_count {
                let cnt = (args.input_count - offset).min(once_max_reg_cnt);
                let addr = offset as u16;

                match timeout(IO_TIMEOUT, ctx.read_input_registers(addr, cnt as u16)).await {
                    Ok(Ok(rsp)) => match rsp {
                        Ok(values) => {
                            let mut s = state.write().await;
                            let end = (offset + values.len()).min(s.input_registers.len());
                            let write_len = end.saturating_sub(offset);
                            if write_len > 0 {
                                for i in 0..write_len {
                                    let idx = offset + i;
                                    s.input_registers[idx] = values[i];
                                }
                            }
                            if write_len > 0 {
                                let fi = FrameInfo {
                                    is_tcp: s.is_tcp,
                                    unit: args.unit,
                                    func_code: 0x04,
                                    func_name: t!("func_code.read_input").to_string(),
                                    addr,
                                    values: values[..write_len].to_vec(),
                                    is_request: false,
                                };
                                record_frame(&mut s.monitor, &fi);
                                s.last_frame = Some(fi);
                            }
                            offset += cnt;
                        }
                        Err(e) => {
                            return Err(anyhow!(t!(
                                "modbus.exception_response",
                                err = format!("{:?}", e)
                            )));
                        }
                    },
                    Ok(Err(e)) => {
                        return Err(anyhow!(t!("modbus.client_read_fail", e = e)));
                    }
                    Err(_) => {
                        return Err(anyhow!(t!("modbus.client_read_timeout")));
                    }
                }
            }
        }
    }
}

pub async fn run_modbus_tcp_client(
    args: Args,
    state: Arc<RwLock<AppState>>,
    rx: mpsc::UnboundedReceiver<RegCmd>,
) -> Result<()> {
    let host = if args.device == "dev/null" {
        "127.0.0.1".to_string()
    } else {
        args.device.clone()
    };
    let addr = format!("{}:{}", host, args.tcp_port);
    let stream = timeout(IO_TIMEOUT, tokio::net::TcpStream::connect(&addr))
        .await
        .context("连接 TCP 超时")?
        .context("连接 TCP 失败")?;
    let ctx = tokio_modbus::client::tcp::attach_slave(stream, Slave(args.unit));

    client_read_write_loop(ctx, args, state, rx).await
}

//各模式分支函数
pub async fn run_modbus_tcp_server(
    args: Args,
    state: Arc<RwLock<AppState>>,
    tx: mpsc::UnboundedSender<RegCmd>,
) -> Result<()> {
    let listener = TcpListener::bind(format!("0.0.0.0:{}", args.tcp_port))
        .await
        .context(t!("modbus.open_tcp_port"))?;

    let service = HoldingService {
        tx,
        holding_len: args.holding_count,
        unit: args.unit,
        state,
        is_tcp: true,
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
        .context(t!("modbus.tcp_server_fail"))?;

    Ok(())
}

pub async fn run_modbus_rtu_client(
    args: Args,
    state: Arc<RwLock<AppState>>,
    rx: mpsc::UnboundedReceiver<RegCmd>,
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
        .context(t!("modbus.open_rtu_port"))?;

    let ctx = tokio_modbus::client::rtu::attach_slave(port, slave);

    client_read_write_loop(ctx, args, state, rx).await
}

pub async fn run_modbus_rtu_server(
    args: Args,
    state: Arc<RwLock<AppState>>,
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
        .context(t!("modbus.open_rtu_port"))?;

    let service = HoldingService {
        tx,
        holding_len: args.holding_count,
        unit: args.unit,
        state,
        is_tcp: false,
    };

    let server = server::rtu::Server::new(port);
    server
        .serve_forever(service)
        .await
        .map_err(|e| anyhow!("{e}"))?;
    Ok(())
}

/// Modbus 监听模式（TCP）：连接目标设备并轮询寄存器，记录所有帧
pub async fn run_modbus_monitor_tcp(args: Args, state: Arc<RwLock<AppState>>) -> Result<()> {
    let host = if args.device == "dev/null" {
        "127.0.0.1".to_string()
    } else {
        args.device.clone()
    };
    let addr = format!("{}:{}", host, args.tcp_port);
    let stream = timeout(IO_TIMEOUT, tokio::net::TcpStream::connect(&addr))
        .await
        .context("连接 TCP 超时")?
        .context("连接 TCP 失败")?;
    let mut ctx = tokio_modbus::client::tcp::attach_slave(stream, Slave(args.unit));

    let tick = Duration::from_millis(args.client_tick_ms);
    let mut interval = tokio::time::interval(tick);
    let once_max_reg_cnt: usize = 120;

    // 记录连接事件
    {
        let fi = FrameInfo {
            is_tcp: true,
            unit: args.unit,
            func_code: 0x03,
            func_name: "已连接监听".to_string(),
            addr: 0,
            values: vec![0; args.holding_count.min(once_max_reg_cnt)],
            is_request: false,
        };
        let mut s = state.write().await;
        s.is_tcp = true;
        record_frame(&mut s.monitor, &fi);
        s.last_frame = Some(fi);
    }

    loop {
        interval.tick().await;

        // 轮询读取
        let mut offset: usize = 0;
        while offset < args.holding_count {
            let cnt = (args.holding_count - offset).min(once_max_reg_cnt);
            let read_addr = offset as u16;

            match timeout(
                IO_TIMEOUT,
                ctx.read_holding_registers(read_addr, cnt as u16),
            )
            .await
            {
                Ok(Ok(rsp)) => match rsp {
                    Ok(values) => {
                        let mut s = state.write().await;
                        let end = (offset + values.len()).min(s.holding.len());
                        let write_len = end.saturating_sub(offset);
                        if write_len > 0 {
                            for i in 0..write_len {
                                let idx = offset + i;
                                let old = s.holding[idx];
                                let new = values[i];
                                if old != new {
                                    s.holding[idx] = new;
                                    crate::record_reg_change(&mut s, idx, old, new);
                                }
                            }
                        }
                        if write_len > 0 {
                            let fi = FrameInfo {
                                is_tcp: true,
                                unit: args.unit,
                                func_code: 0x03,
                                func_name: "读保持寄存器".to_string(),
                                addr: read_addr,
                                values: values[..write_len].to_vec(),
                                is_request: false,
                            };
                            record_frame(&mut s.monitor, &fi);
                            s.last_frame = Some(fi);
                        }
                        offset += cnt;
                    }
                    Err(e) => {
                        let fi = FrameInfo {
                            is_tcp: true,
                            unit: args.unit,
                            func_code: 0x03,
                            func_name: format!("异常: {:?}", e),
                            addr: read_addr,
                            values: vec![],
                            is_request: false,
                        };
                        let mut s = state.write().await;
                        record_frame(&mut s.monitor, &fi);
                        s.last_frame = Some(fi);
                        break;
                    }
                },
                Ok(Err(e)) => {
                    let fi = FrameInfo {
                        is_tcp: true,
                        unit: args.unit,
                        func_code: 0x03,
                        func_name: format!("读取失败: {}", e),
                        addr: read_addr,
                        values: vec![],
                        is_request: false,
                    };
                    let mut s = state.write().await;
                    record_frame(&mut s.monitor, &fi);
                    s.last_frame = Some(fi);
                    break;
                }
                Err(_) => {
                    let fi = FrameInfo {
                        is_tcp: true,
                        unit: args.unit,
                        func_code: 0x03,
                        func_name: "读取超时 (Timeout)".to_string(),
                        addr: read_addr,
                        values: vec![],
                        is_request: false,
                    };
                    let mut s = state.write().await;
                    record_frame(&mut s.monitor, &fi);
                    s.last_frame = Some(fi);
                    break;
                }
            }
        }
    }
}

/// 根据 FrameInfo 构造原始的 Modbus 帧字节（用于 UI 展示）
pub fn frame_bytes_from_info(fi: &FrameInfo) -> Vec<u8> {
    let is_tcp = fi.is_tcp;
    let unit = fi.unit;
    match fi.func_code {
        0x01 | 0x02 => {
            // 读线圈(0x01) / 读离散输入(0x02) 响应
            let mut bytes: Vec<u8> = Vec::new();
            let mut raw = Vec::new();
            raw.push(fi.func_code);
            let bit_count = fi.values.len();
            let byte_count = (bit_count + 7) / 8;
            raw.push(byte_count as u8);
            // 将 bool 值打包为字节
            let mut packed = vec![0u8; byte_count];
            for (i, &v) in fi.values.iter().enumerate() {
                if v != 0 {
                    packed[i / 8] |= 1 << (i % 8);
                }
            }
            raw.extend_from_slice(&packed);
            if is_tcp {
                bytes.extend_from_slice(&[0x00, 0x01, 0x00, 0x00]);
                bytes.push(0x00);
                bytes.push(raw.len() as u8 + 1);
                bytes.push(unit);
                bytes.extend_from_slice(&raw);
            } else {
                bytes.push(unit);
                bytes.extend_from_slice(&raw);
                let crc = calc_crc16(&bytes);
                bytes.push((crc & 0xFF) as u8);
                bytes.push((crc >> 8) as u8);
            }
            bytes
        }
        0x03 => {
            // 读保持寄存器响应
            let mut bytes: Vec<u8> = Vec::new();
            let mut raw = Vec::new();
            raw.push(fi.func_code);
            let payload_len = fi.values.len() * 2;
            raw.push(payload_len as u8);
            for v in &fi.values {
                raw.push((v >> 8) as u8);
                raw.push((v & 0xFF) as u8);
            }
            if is_tcp {
                bytes.extend_from_slice(&[0x00, 0x01, 0x00, 0x00]);
                bytes.push(0x00);
                bytes.push(raw.len() as u8 + 1);
                bytes.push(unit);
                bytes.extend_from_slice(&raw);
            } else {
                bytes.push(unit);
                bytes.extend_from_slice(&raw);
                let crc = calc_crc16(&bytes);
                bytes.push((crc & 0xFF) as u8);
                bytes.push((crc >> 8) as u8);
            }
            bytes
        }
        0x04 => {
            // 读输入寄存器响应
            let mut bytes: Vec<u8> = Vec::new();
            let mut raw = Vec::new();
            raw.push(fi.func_code);
            let payload_len = fi.values.len() * 2;
            raw.push(payload_len as u8);
            for v in &fi.values {
                raw.push((v >> 8) as u8);
                raw.push((v & 0xFF) as u8);
            }
            if is_tcp {
                bytes.extend_from_slice(&[0x00, 0x01, 0x00, 0x00]);
                bytes.push(0x00);
                bytes.push(raw.len() as u8 + 1);
                bytes.push(unit);
                bytes.extend_from_slice(&raw);
            } else {
                bytes.push(unit);
                bytes.extend_from_slice(&raw);
                let crc = calc_crc16(&bytes);
                bytes.push((crc & 0xFF) as u8);
                bytes.push((crc >> 8) as u8);
            }
            bytes
        }
        0x05 => {
            // 写单线圈响应 (echo: addr + 0xFF00/0x0000)
            let mut bytes: Vec<u8> = Vec::new();
            let mut raw = Vec::new();
            let addr = fi.addr;
            raw.push(fi.func_code);
            raw.push((addr >> 8) as u8);
            raw.push((addr & 0xFF) as u8);
            if let Some(&v) = fi.values.first() {
                if v != 0 {
                    raw.push(0xFF);
                    raw.push(0x00);
                } else {
                    raw.push(0x00);
                    raw.push(0x00);
                }
            }
            if is_tcp {
                bytes.extend_from_slice(&[0x00, 0x01, 0x00, 0x00]);
                bytes.push(0x00);
                bytes.push(raw.len() as u8 + 1);
                bytes.push(unit);
                bytes.extend_from_slice(&raw);
            } else {
                bytes.push(unit);
                bytes.extend_from_slice(&raw);
                let crc = calc_crc16(&bytes);
                bytes.push((crc & 0xFF) as u8);
                bytes.push((crc >> 8) as u8);
            }
            bytes
        }
        0x06 => {
            // 写单寄存器响应
            let mut bytes: Vec<u8> = Vec::new();
            let mut raw = Vec::new();
            let addr = fi.addr;
            raw.push(fi.func_code);
            raw.push((addr >> 8) as u8);
            raw.push((addr & 0xFF) as u8);
            if let Some(v) = fi.values.first() {
                raw.push((v >> 8) as u8);
                raw.push((v & 0xFF) as u8);
            }
            if is_tcp {
                bytes.extend_from_slice(&[0x00, 0x01, 0x00, 0x00]);
                bytes.push(0x00);
                bytes.push(raw.len() as u8 + 1);
                bytes.push(unit);
                bytes.extend_from_slice(&raw);
            } else {
                bytes.push(unit);
                bytes.extend_from_slice(&raw);
                let crc = calc_crc16(&bytes);
                bytes.push((crc & 0xFF) as u8);
                bytes.push((crc >> 8) as u8);
            }
            bytes
        }
        0x0F => {
            // 写多线圈响应
            let mut bytes: Vec<u8> = Vec::new();
            let addr = fi.addr;
            let qty = fi.values.len() as u16;
            let mut raw = Vec::new();
            raw.push(fi.func_code);
            raw.push((addr >> 8) as u8);
            raw.push((addr & 0xFF) as u8);
            raw.push((qty >> 8) as u8);
            raw.push((qty & 0xFF) as u8);
            if is_tcp {
                bytes.extend_from_slice(&[0x00, 0x01, 0x00, 0x00]);
                bytes.push(0x00);
                bytes.push(raw.len() as u8 + 1);
                bytes.push(unit);
                bytes.extend_from_slice(&raw);
            } else {
                bytes.push(unit);
                bytes.extend_from_slice(&raw);
                let crc = calc_crc16(&bytes);
                bytes.push((crc & 0xFF) as u8);
                bytes.push((crc >> 8) as u8);
            }
            bytes
        }
        0x10 => {
            // 写多寄存器响应
            let mut bytes: Vec<u8> = Vec::new();
            let addr = fi.addr;
            let qty = fi.values.len() as u16;
            let mut raw = Vec::new();
            raw.push(fi.func_code);
            raw.push((addr >> 8) as u8);
            raw.push((addr & 0xFF) as u8);
            raw.push((qty >> 8) as u8);
            raw.push((qty & 0xFF) as u8);
            if is_tcp {
                bytes.extend_from_slice(&[0x00, 0x01, 0x00, 0x00]);
                bytes.push(0x00);
                bytes.push(raw.len() as u8 + 1);
                bytes.push(unit);
                bytes.extend_from_slice(&raw);
            } else {
                bytes.push(unit);
                bytes.extend_from_slice(&raw);
                let crc = calc_crc16(&bytes);
                bytes.push((crc & 0xFF) as u8);
                bytes.push((crc >> 8) as u8);
            }
            bytes
        }
        _ => vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AppState;
    use std::sync::Arc;
    use tokio::sync::{mpsc, oneshot, RwLock};

    /// 辅助函数：创建一个带 n 个寄存器的测试 AppState
    fn test_state(holding: usize, coils: usize, discrete: usize, input: usize) -> AppState {
        AppState {
            holding: vec![0u16; holding],
            holding_label: vec!["".to_string(); holding],
            coils: vec![false; coils],
            discrete: vec![false; discrete],
            input_registers: vec![0u16; input],
            last_frame: None,
            is_tcp: false,
            monitor: crate::MonitorStats::default(),
            stability_test_running: false,
            stability_stats: (0, 0, 0),
            reg_change_history: Vec::new(),
            reg_just_changed: vec![false; holding],
            reg_change_direction: vec![crate::ChangeDirection::Up; holding],
            holding_change_enabled: vec![false; holding],
            input_change_enabled: vec![false; input],
            holding_change_patterns: vec![crate::RegChangePattern::Random; holding],
            input_change_patterns: vec![crate::RegChangePattern::Random; input],
            holding_pattern_freqs: vec![1.0; holding],
            input_pattern_freqs: vec![1.0; input],
            holding_pattern_phases: vec![0.0; holding],
            input_pattern_phases: vec![0.0; input],
            read_enabled: [false, false, false, false],
            slave_scan_result: None,
            slave_scan_running: false,
            reg_bar_history: vec![Vec::new(); holding],
            reg_format: crate::RegDataFormat::U16,
            swap_bytes: false,
            swap_words: false,
        }
    }

    // ─── 读/写保持寄存器 ───

    #[tokio::test]
    async fn test_read_holding_ok() {
        let state = Arc::new(RwLock::new(test_state(10, 0, 0, 0)));
        {
            let mut s = state.write().await;
            s.holding[3] = 42;
        }
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(reg_worker_loop(Arc::clone(&state), rx));

        let (resp_tx, resp_rx) = oneshot::channel();
        tx.send(RegCmd::ReadHolding {
            addr: 3,
            cnt: 2,
            resp: resp_tx,
        })
        .unwrap();
        let result = resp_rx.await.unwrap().unwrap();
        assert_eq!(result, vec![42u16, 0]);
    }

    #[tokio::test]
    async fn test_read_holding_oob() {
        let state = Arc::new(RwLock::new(test_state(5, 0, 0, 0)));
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(reg_worker_loop(Arc::clone(&state), rx));

        let (resp_tx, resp_rx) = oneshot::channel();
        tx.send(RegCmd::ReadHolding {
            addr: 3,
            cnt: 5,
            resp: resp_tx,
        })
        .unwrap();
        let err = resp_rx.await.unwrap().unwrap_err();
        assert_eq!(err, ExceptionCode::IllegalDataAddress);
    }

    #[tokio::test]
    async fn test_write_single_holding() {
        let state = Arc::new(RwLock::new(test_state(5, 0, 0, 0)));
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(reg_worker_loop(Arc::clone(&state), rx));

        let (resp_tx, resp_rx) = oneshot::channel();
        tx.send(RegCmd::WriteSingleHolding {
            addr: 2,
            value: 1234,
            resp: resp_tx,
        })
        .unwrap();
        resp_rx.await.unwrap().unwrap();
        assert_eq!(state.read().await.holding[2], 1234);
    }

    #[tokio::test]
    async fn test_write_multiple_holding() {
        let state = Arc::new(RwLock::new(test_state(10, 0, 0, 0)));
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(reg_worker_loop(Arc::clone(&state), rx));

        let values = vec![10, 20, 30];
        let (resp_tx, resp_rx) = oneshot::channel();
        tx.send(RegCmd::WriteMultipleHolding {
            addr: 1,
            values,
            resp: resp_tx,
        })
        .unwrap();
        resp_rx.await.unwrap().unwrap();
        let s = state.read().await;
        assert_eq!(s.holding[1], 10);
        assert_eq!(s.holding[2], 20);
        assert_eq!(s.holding[3], 30);
    }

    // ─── 读/写线圈 ───

    #[tokio::test]
    async fn test_read_coils_ok() {
        let state = Arc::new(RwLock::new(test_state(0, 8, 0, 0)));
        {
            let mut s = state.write().await;
            s.coils[1] = true;
            s.coils[3] = true;
        }
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(reg_worker_loop(Arc::clone(&state), rx));

        let (resp_tx, resp_rx) = oneshot::channel();
        tx.send(RegCmd::ReadCoils {
            addr: 0,
            cnt: 4,
            resp: resp_tx,
        })
        .unwrap();
        let result = resp_rx.await.unwrap().unwrap();
        assert_eq!(result, vec![false, true, false, true]);
    }

    #[tokio::test]
    async fn test_write_single_coil() {
        let state = Arc::new(RwLock::new(test_state(0, 5, 0, 0)));
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(reg_worker_loop(Arc::clone(&state), rx));

        let (resp_tx, resp_rx) = oneshot::channel();
        tx.send(RegCmd::WriteSingleCoil {
            addr: 2,
            value: true,
            resp: resp_tx,
        })
        .unwrap();
        resp_rx.await.unwrap().unwrap();
        assert!(state.read().await.coils[2]);
    }

    #[tokio::test]
    async fn test_write_multiple_coils() {
        let state = Arc::new(RwLock::new(test_state(0, 6, 0, 0)));
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(reg_worker_loop(Arc::clone(&state), rx));

        let values = vec![true, false, true];
        let (resp_tx, resp_rx) = oneshot::channel();
        tx.send(RegCmd::WriteMultipleCoils {
            addr: 1,
            values,
            resp: resp_tx,
        })
        .unwrap();
        resp_rx.await.unwrap().unwrap();
        let s = state.read().await;
        assert!(!s.coils[0]);
        assert!(s.coils[1]);
        assert!(!s.coils[2]);
        assert!(s.coils[3]);
    }

    #[tokio::test]
    async fn test_coil_write_oob() {
        let state = Arc::new(RwLock::new(test_state(0, 3, 0, 0)));
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(reg_worker_loop(Arc::clone(&state), rx));

        let (resp_tx, resp_rx) = oneshot::channel();
        tx.send(RegCmd::WriteSingleCoil {
            addr: 10,
            value: true,
            resp: resp_tx,
        })
        .unwrap();
        let err = resp_rx.await.unwrap().unwrap_err();
        assert_eq!(err, ExceptionCode::IllegalDataAddress);
    }

    // ─── 读离散输入 ───

    #[tokio::test]
    async fn test_read_discrete_inputs() {
        let state = Arc::new(RwLock::new(test_state(0, 0, 4, 0)));
        {
            let mut s = state.write().await;
            s.discrete[0] = true;
            s.discrete[2] = true;
        }
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(reg_worker_loop(Arc::clone(&state), rx));

        let (resp_tx, resp_rx) = oneshot::channel();
        tx.send(RegCmd::ReadDiscreteInputs {
            addr: 0,
            cnt: 3,
            resp: resp_tx,
        })
        .unwrap();
        let result = resp_rx.await.unwrap().unwrap();
        assert_eq!(result, vec![true, false, true]);
    }

    // ─── 读输入寄存器 ───

    #[tokio::test]
    async fn test_read_input_registers() {
        let state = Arc::new(RwLock::new(test_state(0, 0, 0, 5)));
        {
            let mut s = state.write().await;
            s.input_registers[1] = 77;
            s.input_registers[3] = 99;
        }
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(reg_worker_loop(Arc::clone(&state), rx));

        let (resp_tx, resp_rx) = oneshot::channel();
        tx.send(RegCmd::ReadInputRegisters {
            addr: 1,
            cnt: 3,
            resp: resp_tx,
        })
        .unwrap();
        let result = resp_rx.await.unwrap().unwrap();
        assert_eq!(result, vec![77, 0, 99]);
    }

    // ─── OOB 边界测试 ───

    #[tokio::test]
    async fn test_all_oob() {
        let state = Arc::new(RwLock::new(test_state(3, 3, 3, 3)));
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(reg_worker_loop(Arc::clone(&state), rx));

        // ReadHolding OOB
        let (rtx, rrx) = oneshot::channel();
        tx.send(RegCmd::ReadHolding {
            addr: 0,
            cnt: 5,
            resp: rtx,
        })
        .unwrap();
        assert_eq!(
            rrx.await.unwrap().unwrap_err(),
            ExceptionCode::IllegalDataAddress
        );

        // ReadCoils OOB
        let (rtx, rrx) = oneshot::channel();
        tx.send(RegCmd::ReadCoils {
            addr: 0,
            cnt: 5,
            resp: rtx,
        })
        .unwrap();
        assert_eq!(
            rrx.await.unwrap().unwrap_err(),
            ExceptionCode::IllegalDataAddress
        );

        // WriteMultipleCoils OOB
        let (rtx, rrx) = oneshot::channel();
        tx.send(RegCmd::WriteMultipleCoils {
            addr: 1,
            values: vec![true; 5],
            resp: rtx,
        })
        .unwrap();
        assert_eq!(
            rrx.await.unwrap().unwrap_err(),
            ExceptionCode::IllegalDataAddress
        );

        // ReadDiscreteInputs OOB
        let (rtx, rrx) = oneshot::channel();
        tx.send(RegCmd::ReadDiscreteInputs {
            addr: 0,
            cnt: 5,
            resp: rtx,
        })
        .unwrap();
        assert_eq!(
            rrx.await.unwrap().unwrap_err(),
            ExceptionCode::IllegalDataAddress
        );

        // ReadInputRegisters OOB
        let (rtx, rrx) = oneshot::channel();
        tx.send(RegCmd::ReadInputRegisters {
            addr: 0,
            cnt: 5,
            resp: rtx,
        })
        .unwrap();
        assert_eq!(
            rrx.await.unwrap().unwrap_err(),
            ExceptionCode::IllegalDataAddress
        );
    }

    // ─── CRC16 测试 ───

    #[test]
    fn test_crc16() {
        // Modbus RTU 示例: 01 03 00 00 00 01 → CRC 0x840A
        let data = [0x01, 0x03, 0x00, 0x00, 0x00, 0x01];
        let crc = calc_crc16(&data);
        assert_eq!(crc, 0x0A84); // 原始 CRC 值（非帧内字节序）
    }

    // ─── Frame bytes ───

    #[test]
    fn test_frame_bytes_read_coils_rtu() {
        let fi = crate::FrameInfo {
            is_tcp: false,
            unit: 1,
            func_code: 0x01,
            func_name: "读线圈".into(),
            addr: 0,
            values: vec![1, 0, 1, 0, 0, 0, 0, 0], // 8 bits → 1 byte: 0b00000101
            is_request: false,
        };
        let bytes = frame_bytes_from_info(&fi);
        assert!(!bytes.is_empty());
        // 第1字节: 从站地址 (01)
        assert_eq!(bytes[0], 0x01);
        // 第2字节: 功能码 (0x01)
        assert_eq!(bytes[1], 0x01);
        // 第3字节: 后续字节数 (1)
        assert_eq!(bytes[2], 0x01);
        // 第4字节: 线圈数据 (bit0=1, bit2=1 → 0b00000101 = 0x05)
        assert_eq!(bytes[3], 0x05);
        // 最后2字节: CRC
        assert!(bytes.len() >= 5);
    }

    #[test]
    fn test_frame_bytes_read_holding_rtu() {
        let fi = crate::FrameInfo {
            is_tcp: false,
            unit: 1,
            func_code: 0x03,
            func_name: "读保持寄存器".into(),
            addr: 0,
            values: vec![0x1234, 0xABCD],
            is_request: false,
        };
        let bytes = frame_bytes_from_info(&fi);
        assert!(!bytes.is_empty());
        assert_eq!(bytes[0], 0x01); // unit
        assert_eq!(bytes[1], 0x03); // func
        assert_eq!(bytes[2], 4); // byte count (2 regs × 2 bytes)
        assert_eq!(bytes[3], 0x12);
        assert_eq!(bytes[4], 0x34);
        assert_eq!(bytes[5], 0xAB);
        assert_eq!(bytes[6], 0xCD);
        // CRC present
        assert!(bytes.len() >= 8);
    }

    #[test]
    fn test_frame_bytes_write_single_coil_rtu() {
        let fi = crate::FrameInfo {
            is_tcp: false,
            unit: 1,
            func_code: 0x05,
            func_name: "写单线圈".into(),
            addr: 5,
            values: vec![0xFF00],
            is_request: false,
        };
        let bytes = frame_bytes_from_info(&fi);
        assert!(!bytes.is_empty());
        assert_eq!(bytes[0], 0x01); // unit
        assert_eq!(bytes[1], 0x05); // func
        assert_eq!(bytes[2], 0x00); // addr high
        assert_eq!(bytes[3], 0x05); // addr low
        assert_eq!(bytes[4], 0xFF); // value high
        assert_eq!(bytes[5], 0x00); // value low
    }

    #[test]
    fn test_frame_bytes_write_multiple_coils_rtu() {
        let fi = crate::FrameInfo {
            is_tcp: false,
            unit: 1,
            func_code: 0x0F,
            func_name: "写多线圈".into(),
            addr: 10,
            values: vec![1, 0, 1], // 3 coils
            is_request: false,
        };
        let bytes = frame_bytes_from_info(&fi);
        assert!(!bytes.is_empty());
        assert_eq!(bytes[0], 0x01); // unit
        assert_eq!(bytes[1], 0x0F); // func
        assert_eq!(bytes[2], 0x00); // addr high
        assert_eq!(bytes[3], 0x0A); // addr low (10)
        assert_eq!(bytes[4], 0x00); // qty high
        assert_eq!(bytes[5], 0x03); // qty low (3)
    }

    #[test]
    fn test_frame_bytes_read_input_rtu() {
        let fi = crate::FrameInfo {
            is_tcp: false,
            unit: 2,
            func_code: 0x04,
            func_name: "读输入寄存器".into(),
            addr: 0,
            values: vec![0xDEAD],
            is_request: false,
        };
        let bytes = frame_bytes_from_info(&fi);
        assert!(!bytes.is_empty());
        assert_eq!(bytes[0], 0x02); // unit
        assert_eq!(bytes[1], 0x04); // func
        assert_eq!(bytes[2], 2); // byte count
        assert_eq!(bytes[3], 0xDE);
        assert_eq!(bytes[4], 0xAD);
    }

    #[test]
    fn test_frame_bytes_read_discrete_rtu() {
        let fi = crate::FrameInfo {
            is_tcp: false,
            unit: 3,
            func_code: 0x02,
            func_name: "读离散输入".into(),
            addr: 0,
            values: vec![0, 1, 0, 0, 0, 0, 0, 0], // 8 bits → bit1=1
            is_request: false,
        };
        let bytes = frame_bytes_from_info(&fi);
        assert!(!bytes.is_empty());
        assert_eq!(bytes[0], 0x03);
        assert_eq!(bytes[1], 0x02);
        assert_eq!(bytes[2], 1); // byte count
        assert_eq!(bytes[3], 0x02); // bit1=1 → 0b00000010
    }

    #[test]
    fn test_frame_bytes_tcp_format() {
        let fi = crate::FrameInfo {
            is_tcp: true,
            unit: 1,
            func_code: 0x03,
            func_name: "Read Holding Registers".into(),
            addr: 0,
            values: vec![0x0001],
            is_request: false,
        };
        let bytes = frame_bytes_from_info(&fi);
        assert!(!bytes.is_empty());
        // TCP MBAP header: [0x00, 0x01, 0x00, 0x00, len(2 bytes), unit]
        assert_eq!(bytes[0], 0x00); // transaction ID high
        assert_eq!(bytes[1], 0x01); // transaction ID low
        assert_eq!(bytes[2], 0x00); // protocol ID high
        assert_eq!(bytes[3], 0x00); // protocol ID low
        assert_eq!(bytes[5], 0x05); // length (unit(1) + func(1) + bytecount(1) + data(2) = 5)
        assert_eq!(bytes[6], 0x01); // unit
        assert_eq!(bytes[7], 0x03); // func
    }

    /// 测试未知功能码返回空 vec
    #[test]
    fn test_frame_bytes_unknown() {
        let fi = crate::FrameInfo {
            is_tcp: false,
            unit: 1,
            func_code: 0x99,
            func_name: "未知".into(),
            addr: 0,
            values: vec![],
            is_request: false,
        };
        let bytes = frame_bytes_from_info(&fi);
        assert!(bytes.is_empty());
    }
}
