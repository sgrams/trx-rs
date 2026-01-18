// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

#[path = "api.rs"]
mod api;
#[path = "status.rs"]
pub mod status;

use std::net::SocketAddr;

use actix_web::dev::Server;
use actix_web::{web, App, HttpServer};
use tokio::signal;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tracing::{error, info};

use trx_core::RigRequest;
use trx_core::RigState;
use trx_frontend::FrontendSpawner;

/// HTTP frontend implementation.
pub struct HttpFrontend;

impl FrontendSpawner for HttpFrontend {
    fn spawn_frontend(
        state_rx: watch::Receiver<RigState>,
        rig_tx: mpsc::Sender<RigRequest>,
        callsign: Option<String>,
        listen_addr: SocketAddr,
    ) -> JoinHandle<()> {
        tokio::spawn(async move {
            if let Err(e) = serve(listen_addr, state_rx, rig_tx, callsign).await {
                error!("HTTP status server error: {:?}", e);
            }
        })
    }
}

async fn serve(
    addr: SocketAddr,
    state_rx: watch::Receiver<RigState>,
    rig_tx: mpsc::Sender<RigRequest>,
    callsign: Option<String>,
) -> Result<(), actix_web::Error> {
    let server = build_server(addr, state_rx, rig_tx, callsign)?;
    let handle = server.handle();
    tokio::spawn(async move {
        let _ = signal::ctrl_c().await;
        handle.stop(false).await;
    });
    info!("http frontend listening on {}", addr);
    info!("http frontend ready (status/control)");
    server.await?;
    Ok(())
}

fn build_server(
    addr: SocketAddr,
    state_rx: watch::Receiver<RigState>,
    rig_tx: mpsc::Sender<RigRequest>,
    callsign: Option<String>,
) -> Result<Server, actix_web::Error> {
    let state_data = web::Data::new(state_rx);
    let rig_tx = web::Data::new(rig_tx);
    let callsign = web::Data::new(callsign);

    let server = HttpServer::new(move || {
        App::new()
            .app_data(state_data.clone())
            .app_data(rig_tx.clone())
            .app_data(callsign.clone())
            .configure(api::configure)
    })
    .shutdown_timeout(1)
    .disable_signals()
    .bind(addr)?
    .run();
    Ok(server)
}

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.configure(api::configure);
}
