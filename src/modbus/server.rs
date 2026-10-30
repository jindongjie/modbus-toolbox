use anyhow::{anyhow, Context, Result};
use crate::{parse_databits, parse_flow, parse_parity, parse_stopbits};
use crate::{Args, AppState};
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::{mpsc, RwLock};
use tokio_modbus::server;
use tokio_modbus::server::tcp::accept_tcp_connection;
use tokio_serial::SerialPortBuilderExt;

use super::service::HoldingService;
use super::RegCmd;

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
