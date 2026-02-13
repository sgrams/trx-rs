// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

#[path = "api.rs"]
mod api;
#[path = "audio.rs"]
pub mod audio;
#[path = "status.rs"]
pub mod status;
#[path = "auth.rs"]
pub mod auth;

use std::net::SocketAddr;
use std::sync::atomic::AtomicUsize;
use std::sync::Arc;
use std::time::Duration;

use actix_web::dev::Server;
use actix_web::{web, App, HttpServer, middleware::Logger};
use tokio::signal;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tracing::{error, info};

use trx_core::RigRequest;
use trx_core::RigState;
use trx_frontend::{FrontendRuntimeContext, FrontendSpawner};

use auth::{AuthConfig, AuthState, SameSite};

/// HTTP frontend implementation.
pub struct HttpFrontend;

impl FrontendSpawner for HttpFrontend {
    fn spawn_frontend(
        state_rx: watch::Receiver<RigState>,
        rig_tx: mpsc::Sender<RigRequest>,
        callsign: Option<String>,
        listen_addr: SocketAddr,
        context: Arc<FrontendRuntimeContext>,
    ) -> JoinHandle<()> {
        tokio::spawn(async move {
            if let Err(e) = serve(listen_addr, state_rx, rig_tx, callsign, context).await {
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
    context: Arc<FrontendRuntimeContext>,
) -> Result<(), actix_web::Error> {
    audio::start_decode_history_collector(context.clone());
    let server = build_server(addr, state_rx, rig_tx, callsign, context)?;
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
    _callsign: Option<String>,
    context: Arc<FrontendRuntimeContext>,
) -> Result<Server, actix_web::Error> {
    let state_data = web::Data::new(state_rx);
    let rig_tx = web::Data::new(rig_tx);
    let clients = web::Data::new(Arc::new(AtomicUsize::new(0)));
    let context_data = web::Data::new(context);

    // Create authentication state (default: disabled)
    let auth_config = AuthConfig::new(
        false,  // enabled - disabled by default
        None,   // rx_passphrase
        None,   // control_passphrase
        true,   // tx_access_control_enabled
        Duration::from_secs(480 * 60),  // session_ttl (480 minutes)
        false,  // cookie_secure
        SameSite::Lax,  // cookie_same_site
    );
    let auth_state = web::Data::new(AuthState::new(auth_config.clone()));

    // Spawn session cleanup task if auth is enabled
    if auth_config.enabled {
        let store_cleanup = auth_state.store.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(300)); // 5 minutes
            loop {
                interval.tick().await;
                store_cleanup.cleanup_expired();
            }
        });
    }

    let server = HttpServer::new(move || {
        App::new()
            .app_data(state_data.clone())
            .app_data(rig_tx.clone())
            .app_data(clients.clone())
            .app_data(context_data.clone())
            .app_data(auth_state.clone())
            .wrap(Logger::default())
            .wrap(auth::AuthMiddleware)
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
