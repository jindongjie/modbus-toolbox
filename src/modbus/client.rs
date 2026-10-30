use anyhow::{Context, Result};
use crate::record_frame;
use crate::{parse_databits, parse_flow, parse_parity, parse_stopbits};
use crate::{Args, AppState, FrameInfo};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, RwLock};
use tokio::time::timeout;
use tokio_modbus::prelude::*;
use tokio_serial::SerialPortBuilderExt;

use super::{IO_TIMEOUT, RegCmd};

pub async fn client_read_write_loop(
    mut ctx: tokio_modbus::client::Context,
    args: Args,
    state: Arc<RwLock<AppState>>,
    mut rx: mpsc::UnboundedReceiver<RegCmd>,
) -> Result<()> {
    let tick = Duration::from_millis(args.client_tick_ms);
    let mut interval = tokio::time::interval(tick);
    // 单轮读取耗时若超过 tick，错过的心跳不再补发，使实际刷新率 = max(tick, 单轮耗时)，
    // 而不是 tick + 单轮耗时，避免在慢速串口上被进一步拖慢
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    let once_max_reg_cnt: usize = 120;
    loop {
        interval.tick().await;
        let cycle_start = Instant::now();
        // 本轮启用的寄存器类型中，是否有任意一次读取成功（判断设备是否在响应）
        let mut responded = false;

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
                    // 恢复原始从站地址，防止后续读写使用错误的 ID
                    ctx.set_slave(Slave(args.unit));
                    let _ = resp.send(results);
                }
                _ => {}
            }
        }

        //读取保持寄存器
        if state.read().await.read_enabled[0] {
            let mut offset: usize = 0;
            let mut read_ok = true;

            while offset < args.holding_count && read_ok {
                let cnt = (args.holding_count - offset).min(once_max_reg_cnt);
                let addr = offset as u16;

                match timeout(IO_TIMEOUT, ctx.read_holding_registers(addr, cnt as u16)).await {
                    Ok(Ok(rsp)) => match rsp {
                        Ok(values) => {
                            responded = true;
                            let mut s = state.write().await;
                            let end = (offset + values.len()).min(s.holding.len());
                            let write_len = end.saturating_sub(offset);
                            if write_len > 0 {
                                for (i, &new) in values.iter().enumerate().take(write_len) {
                                    let idx = offset + i;
                                    let old = s.holding[idx];
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
                        Err(_) => {
                            // 异常响应，跳过本次读取，下次 tick 重试
                            read_ok = false;
                        }
                    },
                    Ok(Err(_)) => {
                        // 通讯错误，跳过本次读取，下次 tick 重试
                        read_ok = false;
                    }
                    Err(_) => {
                        // 超时，跳过本次读取，下次 tick 重试
                        read_ok = false;
                    }
                }
            }
        }

        //读取线圈
        if state.read().await.read_enabled[1] {
            let mut offset: usize = 0;
            let mut read_ok = true;
            while offset < args.coil_count && read_ok {
                let cnt = (args.coil_count - offset).min(once_max_reg_cnt);
                let addr = offset as u16;

                match timeout(IO_TIMEOUT, ctx.read_coils(addr, cnt as u16)).await {
                    Ok(Ok(rsp)) => match rsp {
                        Ok(values) => {
                            responded = true;
                            let mut s = state.write().await;
                            let end = (offset + values.len()).min(s.coils.len());
                            let write_len = end.saturating_sub(offset);
                            if write_len > 0 {
                                for (i, &new) in values.iter().enumerate().take(write_len) {
                                    let idx = offset + i;
                                    let old = s.coils[idx];
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
                        Err(_) => {
                            read_ok = false;
                        }
                    },
                    Ok(Err(_)) => {
                        read_ok = false;
                    }
                    Err(_) => {
                        read_ok = false;
                    }
                }
            }
        }

        //读取离散输入
        if state.read().await.read_enabled[2] {
            let mut offset: usize = 0;
            let mut read_ok = true;
            while offset < args.discrete_count && read_ok {
                let cnt = (args.discrete_count - offset).min(once_max_reg_cnt);
                let addr = offset as u16;

                match timeout(IO_TIMEOUT, ctx.read_discrete_inputs(addr, cnt as u16)).await {
                    Ok(Ok(rsp)) => match rsp {
                        Ok(values) => {
                            responded = true;
                            let mut s = state.write().await;
                            let end = (offset + values.len()).min(s.discrete.len());
                            let write_len = end.saturating_sub(offset);
                            if write_len > 0 {
                                for (i, &new) in values.iter().enumerate().take(write_len) {
                                    let idx = offset + i;
                                    s.discrete[idx] = new;
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
                        Err(_) => {
                            read_ok = false;
                        }
                    },
                    Ok(Err(_)) => {
                        read_ok = false;
                    }
                    Err(_) => {
                        read_ok = false;
                    }
                }
            }
        }

        //读取输入寄存器
        if state.read().await.read_enabled[3] {
            let mut offset: usize = 0;
            let mut read_ok = true;
            while offset < args.input_count && read_ok {
                let cnt = (args.input_count - offset).min(once_max_reg_cnt);
                let addr = offset as u16;

                match timeout(IO_TIMEOUT, ctx.read_input_registers(addr, cnt as u16)).await {
                    Ok(Ok(rsp)) => match rsp {
                        Ok(values) => {
                            responded = true;
                            let mut s = state.write().await;
                            let end = (offset + values.len()).min(s.input_registers.len());
                            let write_len = end.saturating_sub(offset);
                            if write_len > 0 {
                                for (i, &new) in values.iter().enumerate().take(write_len) {
                                    let idx = offset + i;
                                    s.input_registers[idx] = new;
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
                        Err(_) => {
                            read_ok = false;
                        }
                    },
                    Ok(Err(_)) => {
                        read_ok = false;
                    }
                    Err(_) => {
                        read_ok = false;
                    }
                }
            }
        }

        // ── 更新 IO 健康状态：启用了读取却连续未收到响应，提示设备无响应/可能断开 ──
        {
            let mut s = state.write().await;
            let enabled = s.read_enabled.iter().any(|&e| e);
            if responded {
                s.io_fail_streak = 0;
                s.io_status = None;
            } else if enabled {
                s.io_fail_streak += 1;
                if s.io_fail_streak >= 2 {
                    s.io_status = Some(t!("run_ui.device_no_response").to_string());
                }
            }
            // 记录本轮实际耗时，供 UI 展示真实刷新率（而非设定的轮询间隔）
            s.last_poll_ms = cycle_start.elapsed().as_millis() as u16;
            // 每轮递增版本：last_poll_ms / io_status 属于可见状态，需触发重绘
            s.bump();
        }
    }
}

pub async fn run_modbus_tcp_client(
    args: Args,
    state: Arc<RwLock<AppState>>,
    rx: mpsc::UnboundedReceiver<RegCmd>,
) -> Result<()> {
    let addr = format!("{}:{}", args.tcp_host, args.tcp_port);
    let stream = timeout(IO_TIMEOUT, tokio::net::TcpStream::connect(&addr))
        .await
        .context("连接 TCP 超时")?
        .context("连接 TCP 失败")?;
    let ctx = tokio_modbus::client::tcp::attach_slave(stream, Slave(args.unit));

    client_read_write_loop(ctx, args, state, rx).await
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
