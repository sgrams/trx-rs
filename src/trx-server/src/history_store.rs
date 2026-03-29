// SPDX-FileCopyrightText: 2026 Stan Grams <sjg@haxx.space>
//
// SPDX-License-Identifier: BSD-2-Clause

//! Persistent decode history storage for trx-server using pickledb.
//!
//! History for all decoder types (AIS, VDES, APRS, CW, FT8, WSPR) is
//! serialised as JSON arrays to `~/.local/cache/trx-rs/history.db` and
//! loaded back on startup, preserving up to 24 hours of decodes across
//! trx-server restarts.  Each rig's keys are prefixed with the rig id
//! (e.g. `"default.ais"`) so multi-rig setups don't collide.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use pickledb::{PickleDb, PickleDbDumpPolicy, SerializationMethod};
use serde::{de::DeserializeOwned, Deserialize, Serialize};

use trx_core::decode::{AisMessage, AprsPacket, CwEvent, Ft8Message, VdesMessage, WsprMessage};

use crate::audio::DecoderHistories;

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

fn load_key<T: DeserializeOwned>(db: &PickleDb, key: &str) -> Vec<(Instant, T)> {
    let now_ms = now_unix_ms();
    let cutoff_ms = now_ms - HISTORY_RETENTION_MS;

    let entries: Vec<StoredEntry<T>> = db.get(key).unwrap_or_default();

    entries
        .into_iter()
        .filter(|e| e.ts_ms >= cutoff_ms)
        .map(|e| {
            let age_ms = now_ms.saturating_sub(e.ts_ms).max(0) as u64;
            // checked_sub returns None when age exceeds system uptime; treat as
            // brand-new so the entry stays visible for another 24 h.
            let instant = Instant::now()
                .checked_sub(Duration::from_millis(age_ms))
                .unwrap_or_else(Instant::now);
            (instant, e.data)
        })
        .collect()
}

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

/// Populate `histories` from the database using `rig_id`-prefixed keys.
pub fn load_all(db: &PickleDb, rig_id: &str, histories: &Arc<DecoderHistories>) {
    let k = |suffix: &str| format!("{}.{}", rig_id, suffix);

    if let Ok(mut h) = histories.ais.lock() {
        for e in load_key::<AisMessage>(db, &k("ais")) {
            h.push_back(e);
        }
    }
    if let Ok(mut h) = histories.vdes.lock() {
        for e in load_key::<VdesMessage>(db, &k("vdes")) {
            h.push_back(e);
        }
    }
    if let Ok(mut h) = histories.aprs.lock() {
        for e in load_key::<AprsPacket>(db, &k("aprs")) {
            h.push_back(e);
        }
    }
    if let Ok(mut h) = histories.cw.lock() {
        for e in load_key::<CwEvent>(db, &k("cw")) {
            h.push_back(e);
        }
    }
    if let Ok(mut h) = histories.ft8.lock() {
        for e in load_key::<Ft8Message>(db, &k("ft8")) {
            h.push_back(e);
        }
    }
    if let Ok(mut h) = histories.wspr.lock() {
        for e in load_key::<WsprMessage>(db, &k("wspr")) {
            h.push_back(e);
        }
    }
}

/// Flush `histories` to the database under `rig_id`-prefixed keys and sync.
///
/// Each history's mutex is held only long enough to clone the data out,
/// so serialization (which may be slow) never blocks concurrent readers.
pub fn flush_all(db: &mut PickleDb, rig_id: &str, histories: &Arc<DecoderHistories>) {
    let k = |suffix: &str| format!("{}.{}", rig_id, suffix);

    if let Ok(h) = histories.ais.lock() {
        let snapshot = h.clone();
        drop(h);
        save_key(db, &k("ais"), &snapshot);
    }
    if let Ok(h) = histories.vdes.lock() {
        let snapshot = h.clone();
        drop(h);
        save_key(db, &k("vdes"), &snapshot);
    }
    if let Ok(h) = histories.aprs.lock() {
        let snapshot = h.clone();
        drop(h);
        save_key(db, &k("aprs"), &snapshot);
    }
    if let Ok(h) = histories.cw.lock() {
        let snapshot = h.clone();
        drop(h);
        save_key(db, &k("cw"), &snapshot);
    }
    if let Ok(h) = histories.ft8.lock() {
        let snapshot = h.clone();
        drop(h);
        save_key(db, &k("ft8"), &snapshot);
    }
    if let Ok(h) = histories.wspr.lock() {
        let snapshot = h.clone();
        drop(h);
        save_key(db, &k("wspr"), &snapshot);
    }
    let _ = db.dump();
}

/// Spawn a Tokio task that flushes all rigs' histories to disk every 60 seconds.
pub fn spawn_flush_task(
    db: Arc<Mutex<PickleDb>>,
    rig_histories: Vec<(String, Arc<DecoderHistories>)>,
) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(60));
        interval.tick().await; // consume the immediate first tick
        loop {
            interval.tick().await;
            if let Ok(mut guard) = db.lock() {
                for (rig_id, histories) in &rig_histories {
                    flush_all(&mut guard, rig_id, histories);
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn now_unix_ms_returns_positive() {
        let ms = now_unix_ms();
        // Should be well past epoch (year 2020+).
        assert!(ms > 1_577_836_800_000);
    }

    #[test]
    fn stored_entry_roundtrip_serde() {
        let entry = StoredEntry {
            ts_ms: 1_700_000_000_000i64,
            data: "test message".to_string(),
        };
        let json = serde_json::to_string(&entry).unwrap();
        let decoded: StoredEntry<String> = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.ts_ms, 1_700_000_000_000);
        assert_eq!(decoded.data, "test message");
    }

    #[test]
    fn save_and_load_key_roundtrip() {
        let dir = std::env::temp_dir().join("trx_history_test");
        let _ = std::fs::create_dir_all(&dir);
        let db_file = dir.join("test.db");
        let mut db = PickleDb::new(
            &db_file,
            PickleDbDumpPolicy::DumpUponRequest,
            SerializationMethod::Json,
        );

        let mut deque = VecDeque::new();
        deque.push_back((Instant::now(), "entry_a".to_string()));
        deque.push_back((Instant::now(), "entry_b".to_string()));

        save_key(&mut db, "test_key", &deque);
        let loaded: Vec<(Instant, String)> = load_key(&db, "test_key");

        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].1, "entry_a");
        assert_eq!(loaded[1].1, "entry_b");

        let _ = std::fs::remove_file(&db_file);
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn load_key_filters_expired_entries() {
        let dir = std::env::temp_dir().join("trx_history_test_expired");
        let _ = std::fs::create_dir_all(&dir);
        let db_file = dir.join("test.db");
        let mut db = PickleDb::new(
            &db_file,
            PickleDbDumpPolicy::DumpUponRequest,
            SerializationMethod::Json,
        );

        // Manually insert an entry with an old timestamp.
        let entries = vec![
            StoredEntry {
                ts_ms: 1_000, // Way in the past
                data: "old".to_string(),
            },
            StoredEntry {
                ts_ms: now_unix_ms(), // Current
                data: "fresh".to_string(),
            },
        ];
        let _ = db.set("expiry_test", &entries);

        let loaded: Vec<(Instant, String)> = load_key(&db, "expiry_test");
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].1, "fresh");

        let _ = std::fs::remove_file(&db_file);
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn load_key_missing_returns_empty() {
        let dir = std::env::temp_dir().join("trx_history_test_missing");
        let _ = std::fs::create_dir_all(&dir);
        let db_file = dir.join("test.db");
        let db = PickleDb::new(
            &db_file,
            PickleDbDumpPolicy::DumpUponRequest,
            SerializationMethod::Json,
        );

        let loaded: Vec<(Instant, String)> = load_key(&db, "nonexistent");
        assert!(loaded.is_empty());

        let _ = std::fs::remove_file(&db_file);
        let _ = std::fs::remove_dir(&dir);
    }
}
