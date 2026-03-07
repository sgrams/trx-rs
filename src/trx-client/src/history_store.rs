// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

//! Persistent decode history storage using pickledb.
//!
//! History for all decoder types (AIS, VDES, APRS, CW, FT8, WSPR) is
//! serialised as JSON arrays to `~/.local/cache/trx-rs/history.db` and
//! loaded back on startup, preserving up to 24 hours of decodes across
//! trx-client restarts.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use pickledb::{PickleDb, PickleDbDumpPolicy, SerializationMethod};
use serde::{de::DeserializeOwned, Deserialize, Serialize};

use trx_core::decode::{AisMessage, AprsPacket, CwEvent, Ft8Message, VdesMessage, WsprMessage};
use trx_frontend::FrontendRuntimeContext;

const HISTORY_RETENTION_MS: i64 = 24 * 60 * 60 * 1_000;

#[derive(Serialize, Deserialize)]
struct StoredEntry<T> {
    ts_ms: i64,
    data: T,
}

fn now_unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(i64::MAX)
}

pub fn db_path() -> PathBuf {
    let base = dirs::cache_dir().unwrap_or_else(|| PathBuf::from("."));
    base.join("trx-rs").join("history.db")
}

/// Open (or create) the history database at the canonical cache path.
pub fn open_db() -> PickleDb {
    let path = db_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    PickleDb::load(
        &path,
        PickleDbDumpPolicy::DumpUponRequest,
        SerializationMethod::Json,
    )
    .unwrap_or_else(|_| {
        PickleDb::new(
            path,
            PickleDbDumpPolicy::DumpUponRequest,
            SerializationMethod::Json,
        )
    })
}

/// Deserialise entries for `key`, discarding anything older than 24 h.
fn load_key<T: DeserializeOwned>(db: &PickleDb, key: &str) -> Vec<(Instant, T)> {
    let now_ms = now_unix_ms();
    let cutoff_ms = now_ms - HISTORY_RETENTION_MS;

    let entries: Vec<StoredEntry<T>> = db.get(key).unwrap_or_default();

    entries
        .into_iter()
        .filter(|e| e.ts_ms >= cutoff_ms)
        .map(|e| {
            let age_ms = now_ms.saturating_sub(e.ts_ms).max(0) as u64;
            // checked_sub returns None when age exceeds system uptime; in that
            // case treat the entry as brand-new so it stays visible for 24 h.
            let instant = Instant::now()
                .checked_sub(Duration::from_millis(age_ms))
                .unwrap_or_else(Instant::now);
            (instant, e.data)
        })
        .collect()
}

/// Serialise a VecDeque of history entries into the database under `key`.
fn save_key<T: Clone + Serialize>(db: &mut PickleDb, key: &str, deque: &VecDeque<(Instant, T)>) {
    let now_ms = now_unix_ms();
    let entries: Vec<StoredEntry<T>> = deque
        .iter()
        .map(|(inst, data)| StoredEntry {
            ts_ms: now_ms - inst.elapsed().as_millis() as i64,
            data: data.clone(),
        })
        .collect();
    let _ = db.set(key, &entries);
}

/// Populate all history VecDeques in `ctx` from the database.
pub fn load_all(db: &PickleDb, ctx: &mut FrontendRuntimeContext) {
    if let Ok(mut h) = ctx.ais_history.lock() {
        for e in load_key::<AisMessage>(db, "ais") {
            h.push_back(e);
        }
    }
    if let Ok(mut h) = ctx.vdes_history.lock() {
        for e in load_key::<VdesMessage>(db, "vdes") {
            h.push_back(e);
        }
    }
    if let Ok(mut h) = ctx.aprs_history.lock() {
        for e in load_key::<AprsPacket>(db, "aprs") {
            h.push_back(e);
        }
    }
    if let Ok(mut h) = ctx.cw_history.lock() {
        for e in load_key::<CwEvent>(db, "cw") {
            h.push_back(e);
        }
    }
    if let Ok(mut h) = ctx.ft8_history.lock() {
        for e in load_key::<Ft8Message>(db, "ft8") {
            h.push_back(e);
        }
    }
    if let Ok(mut h) = ctx.wspr_history.lock() {
        for e in load_key::<WsprMessage>(db, "wspr") {
            h.push_back(e);
        }
    }
}

/// Write all in-memory history VecDeques to the database and flush to disk.
pub fn flush_all(db: &mut PickleDb, ctx: &FrontendRuntimeContext) {
    if let Ok(h) = ctx.ais_history.lock() {
        save_key(db, "ais", &h);
    }
    if let Ok(h) = ctx.vdes_history.lock() {
        save_key(db, "vdes", &h);
    }
    if let Ok(h) = ctx.aprs_history.lock() {
        save_key(db, "aprs", &h);
    }
    if let Ok(h) = ctx.cw_history.lock() {
        save_key(db, "cw", &h);
    }
    if let Ok(h) = ctx.ft8_history.lock() {
        save_key(db, "ft8", &h);
    }
    if let Ok(h) = ctx.wspr_history.lock() {
        save_key(db, "wspr", &h);
    }
    let _ = db.dump();
}

/// Spawn a Tokio task that flushes history to disk every 60 seconds.
pub fn spawn_flush_task(db: Arc<Mutex<PickleDb>>, ctx: Arc<FrontendRuntimeContext>) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(60));
        interval.tick().await; // consume the immediate first tick
        loop {
            interval.tick().await;
            if let Ok(mut guard) = db.lock() {
                flush_all(&mut guard, &ctx);
            }
        }
    });
}
