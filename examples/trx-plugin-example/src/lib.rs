// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

use std::net::SocketAddr;

use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tracing::info;

use trx_backend::{RegistrationContext, RigAccess};
use trx_core::{DynResult, RigRequest, RigState};
use trx_frontend::{FrontendRuntimeContext, FrontendSpawner, FrontendRegistrationContext};

const BACKEND_NAME: &str = "example";
const FRONTEND_NAME: &str = "example-frontend";

/// Entry point called by trx-server when the plugin is loaded.
#[no_mangle]
pub extern "C" fn trx_register_backend(context: *mut std::ffi::c_void) {
    let context = unsafe { &mut *(context as *mut RegistrationContext) };
    context.register_backend(BACKEND_NAME, example_backend_factory);
}

/// Entry point called by trx-client when the plugin is loaded.
#[no_mangle]
pub extern "C" fn trx_register_frontend(context: *mut std::ffi::c_void) {
    let context = unsafe { &mut *(context as *mut FrontendRegistrationContext) };
    context.register_frontend(FRONTEND_NAME, ExampleFrontend::spawn_frontend);
}

fn example_backend_factory(_access: RigAccess) -> DynResult<Box<dyn trx_core::rig::RigCat>> {
    Err("example plugin backend not implemented".into())
}

struct ExampleFrontend;

impl FrontendSpawner for ExampleFrontend {
    fn spawn_frontend(
        _state_rx: watch::Receiver<RigState>,
        _rig_tx: mpsc::Sender<RigRequest>,
        _callsign: Option<String>,
        listen_addr: SocketAddr,
        _context: std::sync::Arc<FrontendRuntimeContext>,
    ) -> JoinHandle<()> {
        tokio::spawn(async move {
            info!("example frontend loaded at {} (no-op)", listen_addr);
        })
    }
}
