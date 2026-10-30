use anyhow::{Context, Result};
use crate::record_frame;
use crate::{parse_databits, parse_flow, parse_parity, parse_stopbits};
use crate::{Args, AppState, FrameInfo};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::net::TcpListener;
use tokio::sync::RwLock;
use tokio_serial::SerialPortBuilderExt;

use super::frame::calc_crc16;

/// Modbus 监听模式（TCP）：被动监听，不发送任何数据
/// 绑定为 TCP 服务器，只读取传入的数据流，从不写入。
pub async fn run_modbus_monitor_tcp(args: Args, state: Arc<RwLock<AppState>>) -> Result<()> {
    let bind_addr = format!("0.0.0.0:{}", args.tcp_port);
    let listener = TcpListener::bind(&bind_addr)
        .await
        .with_context(|| format!("监听端口 {} 失败", args.tcp_port))?;

    {
        let mut s = state.write().await;
        s.is_tcp = true;
    }

    loop {
        // 接受新连接
        let (mut stream, peer) = match listener.accept().await {
            Ok(v) => v,
            Err(e) => {
                let fi = FrameInfo {
                    is_tcp: true,
                    unit: 0,
                    func_code: 0,
                    func_name: format!("accept error: {e}"),
                    addr: 0,
                    values: vec![],
                    is_request: false,
                };
                let mut s = state.write().await;
                record_frame(&mut s.monitor, &fi);
                s.last_frame = Some(fi);
                continue;
            }
        };

        // 新连接 → 重置旧记录
        {
            let mut s = state.write().await;
            s.monitor.reset();
            let fi = FrameInfo {
                is_tcp: true,
                unit: 0,
                func_code: 0,
                func_name: format!("TCP connected from {peer}"),
                addr: 0,
                values: vec![],
                is_request: false,
            };
            record_frame(&mut s.monitor, &fi);
            s.last_frame = Some(fi);
        }

        // 只读循环：绝不发送任何数据
        let mut buf = [0u8; 1024];
        loop {
            match stream.read(&mut buf).await {
                Ok(0) => break, // 连接关闭
                Ok(n) => {
                    // 记录接收到的原始字节
                    let raw = buf[..n].to_vec();
                    // 尝试识别 Modbus TCP 帧（至少 8 字节 MBAP + PDU）
                    // 多个帧可能粘在一起，尝试分割
                    let mut offset = 0usize;
                    while offset + 8 <= raw.len() {
                        let len_field =
                            u16::from_be_bytes([raw[offset + 4], raw[offset + 5]]) as usize;
                        let frame_len = 6 + len_field; // MBAP header(6) + unit(1) + PDU
                        if offset + frame_len > raw.len() {
                            break;
                        }
                        let unit = raw[offset + 6];
                        let fc = raw[offset + 7];
                        // 尝试判断是请求还是响应
                        let is_resp = (raw[offset + 7] & 0x80) == 0 && offset > 0;
                        let is_request = !is_resp;
                        let pdu = &raw[offset + 7..offset + frame_len];
                        let addr = if pdu.len() >= 4 {
                            u16::from_be_bytes([pdu[1], pdu[2]])
                        } else {
                            0
                        };
                        let values: Vec<u16> =
                            if (fc == 3 || fc == 4) && !is_request && pdu.len() >= 3 {
                                let bc = pdu[1] as usize;
                                let data = &pdu[2..(2 + bc).min(pdu.len())];
                                data.chunks(2)
                                    .filter_map(|c| {
                                        if c.len() == 2 {
                                            Some(u16::from_be_bytes([c[0], c[1]]))
                                        } else {
                                            None
                                        }
                                    })
                                    .collect()
                            } else {
                                vec![]
                            };
                        let func_name = match fc {
                            1 => "Read Coils",
                            2 => "Read Discrete",
                            3 => "Read Holding",
                            4 => "Read Input",
                            5 => "Write Coil",
                            6 => "Write Reg",
                            15 => "Write Multi Coils",
                            16 => "Write Multi Reg",
                            _ => "Unknown",
                        };
                        let fi = FrameInfo {
                            is_tcp: true,
                            unit,
                            func_code: fc,
                            func_name: func_name.to_string(),
                            addr: addr,
                            values,
                            is_request,
                        };
                        let mut s = state.write().await;
                        record_frame(&mut s.monitor, &fi);
                        s.last_frame = Some(fi);
                        offset += frame_len;
                    }
                    if offset < n {
                        let fi = FrameInfo {
                            is_tcp: true,
                            unit: 0,
                            func_code: 0xFF,
                            func_name: format!("{} unknown bytes discarded", n - offset),
                            addr: 0,
                            values: vec![],
                            is_request: false,
                        };
                        let mut s = state.write().await;
                        record_frame(&mut s.monitor, &fi);
                        s.last_frame = Some(fi);
                    }
                }
                Err(_) => break,
            }
        }

        // 连接断开
        {
            let fi = FrameInfo {
                is_tcp: true,
                unit: 0,
                func_code: 0,
                func_name: format!("TCP disconnected from {peer}"),
                addr: 0,
                values: vec![],
                is_request: false,
            };
            let mut s = state.write().await;
            record_frame(&mut s.monitor, &fi);
            s.last_frame = Some(fi);
        }
    }
}

/// Modbus 监听模式（RTU）：被动监听串口，不发送任何数据
pub async fn run_modbus_monitor_rtu(args: Args, state: Arc<RwLock<AppState>>) -> Result<()> {
    let parity = parse_parity(&args.parity)?;
    let flow = parse_flow(&args.flow)?;
    let databits = parse_databits(args.databits)?;
    let stopbits = parse_stopbits(args.stopbits)?;

    let port = tokio_serial::new(args.device.clone(), args.baudrate)
        .parity(parity)
        .flow_control(flow)
        .data_bits(databits)
        .stop_bits(stopbits)
        .open_native_async()
        .context(t!("modbus.open_rtu_port"))?;

    // 设置为原始只读模式
    let mut reader = tokio::io::BufReader::new(port);

    {
        let mut s = state.write().await;
        s.is_tcp = false;
        s.monitor.reset();
        let fi = FrameInfo {
            is_tcp: false,
            unit: 0,
            func_code: 0,
            func_name: "RTU serial monitor started".to_string(),
            addr: 0,
            values: vec![],
            is_request: false,
        };
        record_frame(&mut s.monitor, &fi);
        s.last_frame = Some(fi);
    }

    // 只读循环：绝不发送任何数据
    let mut raw_buf = Vec::new();
    loop {
        let mut byte = [0u8; 1];
        match reader.read(&mut byte).await {
            Ok(0) => {
                tokio::time::sleep(Duration::from_millis(10)).await;
                continue;
            }
            Ok(_) => {
                raw_buf.push(byte[0]);
                // 简单的帧检测：连续接收，3.5字符静默时间表示帧结束
                // 尝试用 CRC 判断完整帧
                if raw_buf.len() >= 4 {
                    for frame_end in (4..=raw_buf.len().min(256)).rev() {
                        let candidate = &raw_buf[..frame_end];
                        let calc = calc_crc16(&candidate[..candidate.len() - 2]);
                        let stored = u16::from_le_bytes([
                            candidate[candidate.len() - 2],
                            candidate[candidate.len() - 1],
                        ]);
                        if calc == stored {
                            // 有效帧
                            let slave = candidate[0];
                            let fc = candidate[1];
                            let pdu = &candidate[1..candidate.len() - 2];
                            let addr = if pdu.len() >= 4 {
                                u16::from_be_bytes([pdu[1], pdu[2]])
                            } else {
                                0
                            };
                            let is_request = fc & 0x80 == 0;
                            let func_name = match fc & 0x7F {
                                1 => "Read Coils",
                                2 => "Read Discrete",
                                3 => "Read Holding",
                                4 => "Read Input",
                                5 => "Write Coil",
                                6 => "Write Reg",
                                15 => "Write Multi Coils",
                                16 => "Write Multi Reg",
                                _ => "Unknown",
                            };
                            let values: Vec<u16> = if (fc & 0x7F == 3 || fc & 0x7F == 4)
                                && !is_request
                                && pdu.len() >= 3
                            {
                                let bc = pdu[1] as usize;
                                let data = &pdu[2..(2 + bc).min(pdu.len())];
                                data.chunks(2)
                                    .filter_map(|c| {
                                        if c.len() == 2 {
                                            Some(u16::from_be_bytes([c[0], c[1]]))
                                        } else {
                                            None
                                        }
                                    })
                                    .collect()
                            } else {
                                vec![]
                            };
                            let fi = FrameInfo {
                                is_tcp: false,
                                unit: slave,
                                func_code: fc & 0x7F,
                                func_name: func_name.to_string(),
                                addr: addr,
                                values,
                                is_request,
                            };
                            let mut s = state.write().await;
                            record_frame(&mut s.monitor, &fi);
                            s.last_frame = Some(fi);
                            raw_buf.drain(..frame_end);
                            break;
                        }
                    }
                    // 如果缓冲区太大还没有有效帧，丢弃开头部分
                    if raw_buf.len() > 512 {
                        raw_buf.drain(..1);
                    }
                }
            }
            Err(_) => {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }
}
