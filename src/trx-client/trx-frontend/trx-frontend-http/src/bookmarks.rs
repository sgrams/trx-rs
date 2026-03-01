// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use pickledb::{PickleDb, PickleDbDumpPolicy, SerializationMethod};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bookmark {
    pub id: String,
    pub name: String,
    pub freq_hz: u64,
    pub mode: String,
    pub bandwidth_hz: Option<u64>,
    pub comment: String,
    pub category: String,
    pub decoders: Vec<String>,
}

pub struct BookmarkStore {
    db: Arc<RwLock<PickleDb>>,
}

impl BookmarkStore {
    /// Open (or create) the bookmark store at `path`.
    pub fn open(path: &Path) -> Self {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let db = if path.exists() {
            PickleDb::load(path, PickleDbDumpPolicy::AutoDump, SerializationMethod::Json)
                .unwrap_or_else(|_| {
                    PickleDb::new(path, PickleDbDumpPolicy::AutoDump, SerializationMethod::Json)
                })
        } else {
            PickleDb::new(path, PickleDbDumpPolicy::AutoDump, SerializationMethod::Json)
        };
        Self {
            db: Arc::new(RwLock::new(db)),
        }
    }

    /// Returns the platform default path: `~/.config/trx-rs/bookmarks.db`.
    /// Falls back to `./bookmarks.db` when the config dir is unavailable.
    pub fn default_path() -> PathBuf {
        dirs::config_dir()
            .map(|p| p.join("trx-rs").join("bookmarks.db"))
            .unwrap_or_else(|| PathBuf::from("bookmarks.db"))
    }

    pub fn list(&self) -> Vec<Bookmark> {
        let db = self.db.read().unwrap_or_else(|e| e.into_inner());
        db.iter()
            .filter_map(|kv| {
                if kv.get_key().starts_with("bm:") {
                    kv.get_value::<Bookmark>()
                } else {
                    None
                }
            })
            .collect()
    }

    pub fn get(&self, id: &str) -> Option<Bookmark> {
        let db = self.db.read().unwrap_or_else(|e| e.into_inner());
        db.get::<Bookmark>(&format!("bm:{id}"))
    }

    /// Insert a new bookmark. Returns false if the DB write fails.
    pub fn insert(&self, bm: &Bookmark) -> bool {
        let mut db = self.db.write().unwrap_or_else(|e| e.into_inner());
        db.set(&format!("bm:{}", bm.id), bm).is_ok()
    }

    /// Update an existing bookmark by id. Returns false if not found.
    pub fn upsert(&self, id: &str, bm: &Bookmark) -> bool {
        let mut db = self.db.write().unwrap_or_else(|e| e.into_inner());
        let key = format!("bm:{id}");
        if db.exists(&key) {
            db.set(&key, bm).is_ok()
        } else {
            false
        }
    }

    /// Remove a bookmark by id. Returns false if not found.
    pub fn remove(&self, id: &str) -> bool {
        let mut db = self.db.write().unwrap_or_else(|e| e.into_inner());
        db.rem(&format!("bm:{id}")).unwrap_or(false)
    }
}
