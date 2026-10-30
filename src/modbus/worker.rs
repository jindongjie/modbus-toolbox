use crate::AppState;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tokio_modbus::prelude::*;

use super::RegCmd;

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
