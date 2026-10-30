use crate::AppState;
use crate::FrameInfo;
use crate::record_frame;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tokio_modbus::prelude::*;

use super::RegCmd;

//保持型寄存器服务
#[derive(Clone)]
pub(crate) struct HoldingService {
    pub(crate) tx: mpsc::UnboundedSender<RegCmd>,
    pub(crate) holding_len: usize,
    pub(crate) unit: u8,
    pub(crate) state: Arc<RwLock<AppState>>,
    pub(crate) is_tcp: bool,
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
                                func_name: "读保持寄存器".to_string(),
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
                                func_name: "写单寄存器".to_string(),
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
                                func_name: "写多寄存器".to_string(),
                                addr,
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
                                func_name: "读线圈".to_string(),
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
                                func_name: "写单线圈".to_string(),
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
                                func_name: "写多线圈".to_string(),
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
                                func_name: "读离散输入".to_string(),
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
                                func_name: "读输入寄存器".to_string(),
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
