use crate::Args;
use anyhow::{anyhow, Context, Result};
use std::sync::Arc;
use std::time::Duration;
use tokio::time::timeout;
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot, RwLock};
use tokio_modbus::server;
use tokio_modbus::server::tcp::accept_tcp_connection;
use tokio_serial::SerialPortBuilderExt;

use crate::parse_databits;
use crate::parse_flow;
use crate::parse_parity;
use crate::parse_stopbits;
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
}

//寄存器指令执行循环
pub async fn reg_worker_loop(
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
    state: Arc<RwLock<AppState>>,
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

        let is_tcp = self.state.blocking_read().is_tcp;

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
                            state.write().await.last_frame = Some(FrameInfo {
                                is_tcp,
                                unit,
                                func_code: 0x03,
                                func_name: format!("读保持寄存器"),
                                addr: addr as u16,
                                values: values.clone(),
                                is_request: false,
                            });
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
                            state.write().await.last_frame = Some(FrameInfo {
                                is_tcp,
                                unit,
                                func_code: 0x06,
                                func_name: format!("写单寄存器"),
                                addr: addr as u16,
                                values: vec![value],
                                is_request: false,
                            });
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
                            state.write().await.last_frame = Some(FrameInfo {
                                is_tcp,
                                unit,
                                func_code: 0x10,
                                func_name: format!("写多寄存器"),
                                addr: addr as u16,
                                values: values.to_vec(),
                                is_request: false,
                            });
                            Ok(Response::WriteMultipleRegisters(addr, qty))
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
                    let out = match timeout(IO_TIMEOUT, ctx.write_single_register(addr as u16, value)).await {
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

                        let out = match timeout(IO_TIMEOUT, ctx.write_multiple_registers(start as u16, chunk)).await {
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
                _ => {}
            }
        }

        //读取
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
                            s.holding[offset..offset + write_len]
                                .copy_from_slice(&values[..write_len]);
                        }
                        // 更新字节流显示信息
                        if write_len > 0 {
                            s.last_frame = Some(FrameInfo {
                                is_tcp: s.is_tcp,
                                unit: args.unit,
                                func_code: 0x03,
                                func_name: t!("func_code.read_holding").to_string(),
                                addr,
                                values: values[..write_len].to_vec(),
                                is_request: false,
                            });
                        }
                        offset += cnt;
                    }
                    Err(e) => {
                        return Err(anyhow!(t!("modbus.exception_response", err = format!("{:?}", e))));
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
    };

    let server = server::rtu::Server::new(port);
    server
        .serve_forever(service)
        .await
        .map_err(|e| anyhow!("{e}"))?;
    Ok(())
}

/// 根据 FrameInfo 构造原始的 Modbus 帧字节（用于 UI 展示）
pub fn frame_bytes_from_info(fi: &FrameInfo) -> Vec<u8> {
    let is_tcp = fi.is_tcp;
    let unit = fi.unit;
    match fi.func_code {
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
