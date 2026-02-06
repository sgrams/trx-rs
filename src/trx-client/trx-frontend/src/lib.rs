// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Mutex, OnceLock};

use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

use trx_core::{DynResult, RigRequest, RigState};

/// Trait implemented by concrete frontends to expose a runner entrypoint.
pub trait FrontendSpawner {
    fn spawn_frontend(
        state_rx: watch::Receiver<RigState>,
        rig_tx: mpsc::Sender<RigRequest>,
        callsign: Option<String>,
        listen_addr: SocketAddr,
    ) -> JoinHandle<()>;
}

type FrontendSpawnFn = fn(
    watch::Receiver<RigState>,
    mpsc::Sender<RigRequest>,
    Option<String>,
    SocketAddr,
) -> JoinHandle<()>;

struct FrontendRegistry {
    spawners: HashMap<String, FrontendSpawnFn>,
}

impl FrontendRegistry {
    fn new() -> Self {
        Self {
            spawners: HashMap::new(),
        }
    }
}

fn registry() -> &'static Mutex<FrontendRegistry> {
    static REGISTRY: OnceLock<Mutex<FrontendRegistry>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(FrontendRegistry::new()))
}

fn normalize_name(name: &str) -> String {
    name.to_ascii_lowercase()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect()
}

/// Register a frontend spawner under a stable name (e.g. "http").
pub fn register_frontend(name: &str, spawner: FrontendSpawnFn) {
    let key = normalize_name(name);
    let mut reg = registry().lock().expect("frontend registry mutex poisoned");
    reg.spawners.insert(key, spawner);
}

/// Check whether a frontend name is registered.
pub fn is_frontend_registered(name: &str) -> bool {
    let key = normalize_name(name);
    let reg = registry().lock().expect("frontend registry mutex poisoned");
    reg.spawners.contains_key(&key)
}

/// List registered frontend names.
pub fn registered_frontends() -> Vec<String> {
    let reg = registry().lock().expect("frontend registry mutex poisoned");
    let mut names: Vec<String> = reg.spawners.keys().cloned().collect();
    names.sort();
    names
}

/// Spawn a registered frontend by name.
pub fn spawn_frontend(
    name: &str,
    state_rx: watch::Receiver<RigState>,
    rig_tx: mpsc::Sender<RigRequest>,
    callsign: Option<String>,
    listen_addr: SocketAddr,
) -> DynResult<JoinHandle<()>> {
    let key = normalize_name(name);
    let reg = registry().lock().expect("frontend registry mutex poisoned");
    let spawner = reg
        .spawners
        .get(&key)
        .ok_or_else(|| format!("Unknown frontend: {}", name))?;
    Ok(spawner(state_rx, rig_tx, callsign, listen_addr))
}
