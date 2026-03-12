// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

#[path = "api.rs"]
mod api;
#[path = "audio.rs"]
pub mod audio;
#[path = "background_decode.rs"]
pub mod background_decode;
#[path = "auth.rs"]
pub mod auth;
#[path = "bookmarks.rs"]
pub mod bookmarks;
#[path = "scheduler.rs"]
pub mod scheduler;
#[path = "status.rs"]
pub mod status;
#[path = "vchan.rs"]
pub mod vchan;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use actix_web::dev::Server;
use actix_web::{
    middleware::{Compress, DefaultHeaders, Logger},
    web, App, HttpServer,
};
use tokio::signal;
use tokio::sync::{broadcast, mpsc, watch};
use tokio::task::JoinHandle;
use tracing::{error, info};

use trx_core::RigRequest;
use trx_core::RigState;
use trx_frontend::{FrontendRuntimeContext, FrontendSpawner};

use auth::{AuthConfig, AuthState, SameSite};
use background_decode::{BackgroundDecodeManager, BackgroundDecodeStore};
use scheduler::{SchedulerStatusMap, SchedulerStore};
use vchan::ClientChannelManager;

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

    let scheduler_path = SchedulerStore::default_path();
    let scheduler_store = Arc::new(SchedulerStore::open(&scheduler_path));
    let bookmark_path = bookmarks::BookmarkStore::default_path();
    let bookmark_store = Arc::new(bookmarks::BookmarkStore::open(&bookmark_path));
    let scheduler_status: SchedulerStatusMap = Arc::new(RwLock::new(HashMap::new()));

    scheduler::spawn_scheduler_task(
        context.clone(),
        rig_tx.clone(),
        scheduler_store.clone(),
        bookmark_store.clone(),
        scheduler_status.clone(),
    );

    let background_decode_path = BackgroundDecodeStore::default_path();
    let background_decode_store =
        Arc::new(BackgroundDecodeStore::open(&background_decode_path));
    let background_decode_mgr = BackgroundDecodeManager::new(
        background_decode_store,
        bookmark_store.clone(),
        context.clone(),
        scheduler_status.clone(),
    );
    background_decode_mgr.spawn();

    let vchan_mgr = Arc::new(ClientChannelManager::new(4));

    // Wire the audio-command sender so allocate/delete/freq/mode operations on
    // virtual channels are forwarded to the audio-client task.
    if let Ok(guard) = context.vchan_audio_cmd.lock() {
        if let Some(tx) = guard.as_ref() {
            vchan_mgr.set_audio_cmd(tx.clone());
        }
    }

    // Spawn a task that removes channels destroyed server-side (OOB) from the
    // client-side registry so the SSE channel list stays in sync.
    if let Some(ref destroyed_tx) = context.vchan_destroyed {
        let mut destroyed_rx = destroyed_tx.subscribe();
        let mgr_for_destroyed = vchan_mgr.clone();
        tokio::spawn(async move {
            loop {
                match destroyed_rx.recv().await {
                    Ok(uuid) => {
                        mgr_for_destroyed.remove_by_uuid(uuid);
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        });
    }

    let server = build_server(
        addr,
        state_rx,
        rig_tx,
        callsign,
        context,
        bookmark_store,
        scheduler_store,
        scheduler_status,
        vchan_mgr,
        background_decode_mgr,
    )?;
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

#[allow(clippy::too_many_arguments)]
fn build_server(
    addr: SocketAddr,
    state_rx: watch::Receiver<RigState>,
    rig_tx: mpsc::Sender<RigRequest>,
    _callsign: Option<String>,
    context: Arc<FrontendRuntimeContext>,
    bookmark_store: Arc<bookmarks::BookmarkStore>,
    scheduler_store: Arc<SchedulerStore>,
    scheduler_status: SchedulerStatusMap,
    vchan_mgr: Arc<ClientChannelManager>,
    background_decode_mgr: Arc<BackgroundDecodeManager>,
) -> Result<Server, actix_web::Error> {
    let state_data = web::Data::new(state_rx);
    let rig_tx = web::Data::new(rig_tx);
    // Share the same AtomicUsize that lives in FrontendRuntimeContext so the
    // scheduler task can observe the connected-client count.
    let clients = web::Data::new(context.sse_clients.clone());

    let bookmark_store = web::Data::new(bookmark_store);

    let scheduler_store = web::Data::new(scheduler_store);
    let scheduler_status = web::Data::new(scheduler_status);
    let vchan_mgr = web::Data::new(vchan_mgr);
    let background_decode_mgr = web::Data::new(background_decode_mgr);

    // Extract auth config values before moving context
    let same_site = match context.http_auth_cookie_same_site.as_str() {
        "Strict" => SameSite::Strict,
        "None" => SameSite::None,
        _ => SameSite::Lax, // default
    };
    let auth_config = AuthConfig::new(
        context.http_auth_enabled,
        context.http_auth_rx_passphrase.clone(),
        context.http_auth_control_passphrase.clone(),
        context.http_auth_tx_access_control_enabled,
        Duration::from_secs(context.http_auth_session_ttl_secs),
        context.http_auth_cookie_secure,
        same_site,
    );

    let context_data = web::Data::new(context);
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
            .app_data(bookmark_store.clone())
            .app_data(scheduler_store.clone())
            .app_data(scheduler_status.clone())
            .app_data(vchan_mgr.clone())
            .app_data(background_decode_mgr.clone())
            .wrap(Compress::default())
            .wrap(
                DefaultHeaders::new()
                    .add(("Referrer-Policy", "same-origin"))
                    .add(("Cross-Origin-Resource-Policy", "same-origin"))
                    .add(("Cross-Origin-Opener-Policy", "same-origin"))
                    .add(("X-Content-Type-Options", "nosniff")),
            )
            // Use "real IP" so reverse-proxy setups can pass client address
            // via Forwarded / X-Forwarded-For / X-Real-IP headers.
            .wrap(Logger::new(
                r#"%{r}a "%r" %s %b "%{Referer}i" "%{User-Agent}i" %T"#,
            ))
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
