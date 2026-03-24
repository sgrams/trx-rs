// SPDX-FileCopyrightText: 2026 Stan Grams <sjg@haxx.space>
//
// SPDX-License-Identifier: BSD-2-Clause

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

use pickledb::{PickleDb, PickleDbDumpPolicy, SerializationMethod};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bookmark {
    pub id: String,
    pub name: String,
    pub freq_hz: u64,
    pub mode: String,
    pub bandwidth_hz: Option<u64>,
    #[serde(default)]
    pub locator: Option<String>,
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
            PickleDb::load(
                path,
                PickleDbDumpPolicy::AutoDump,
                SerializationMethod::Json,
            )
            .unwrap_or_else(|_| {
                PickleDb::new(
                    path,
                    PickleDbDumpPolicy::AutoDump,
                    SerializationMethod::Json,
                )
            })
        } else {
            PickleDb::new(
                path,
                PickleDbDumpPolicy::AutoDump,
                SerializationMethod::Json,
            )
        };
        Self {
            db: Arc::new(RwLock::new(db)),
        }
    }

    /// General (shared) bookmarks path: `~/.config/trx-rs/bookmarks.db`.
    pub fn general_path() -> PathBuf {
        dirs::config_dir()
            .map(|p| p.join("trx-rs").join("bookmarks.db"))
            .unwrap_or_else(|| PathBuf::from("bookmarks.db"))
    }

    /// Per-rig bookmarks path: `~/.config/trx-rs/bookmark.{remote}.db`.
    pub fn rig_path(remote: &str) -> PathBuf {
        dirs::config_dir()
            .map(|p| p.join("trx-rs").join(format!("bookmark.{remote}.db")))
            .unwrap_or_else(|| PathBuf::from(format!("bookmark.{remote}.db")))
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

    /// Returns true if any bookmark (other than `exclude_id`) has `freq_hz`.
    pub fn freq_taken(&self, freq_hz: u64, exclude_id: Option<&str>) -> bool {
        self.list()
            .into_iter()
            .any(|bm| bm.freq_hz == freq_hz && exclude_id.is_none_or(|ex| bm.id != ex))
    }
}

/// Two-tier bookmark storage: a shared **general** store (`bookmarks.db`)
/// and lazily-opened per-rig stores (`bookmark.{remote}.db`).
pub struct BookmarkStoreMap {
    general: Arc<BookmarkStore>,
    rig_stores: Mutex<HashMap<String, Arc<BookmarkStore>>>,
}

impl Default for BookmarkStoreMap {
    fn default() -> Self {
        Self::new()
    }
}

impl BookmarkStoreMap {
    pub fn new() -> Self {
        let general_path = BookmarkStore::general_path();
        Self {
            general: Arc::new(BookmarkStore::open(&general_path)),
            rig_stores: Mutex::new(HashMap::new()),
        }
    }

    /// The shared general bookmark store.
    pub fn general(&self) -> &Arc<BookmarkStore> {
        &self.general
    }

    /// Return the per-rig store for `remote`, opening it on first access.
    pub fn store_for(&self, remote: &str) -> Arc<BookmarkStore> {
        let mut stores = self.rig_stores.lock().unwrap_or_else(|e| e.into_inner());
        stores
            .entry(remote.to_owned())
            .or_insert_with(|| {
                let path = BookmarkStore::rig_path(remote);
                Arc::new(BookmarkStore::open(&path))
            })
            .clone()
    }

    /// Look up a bookmark by id, checking the rig-specific store first,
    /// then falling back to the general store.
    pub fn get_for_rig(&self, remote: &str, id: &str) -> Option<Bookmark> {
        self.store_for(remote)
            .get(id)
            .or_else(|| self.general.get(id))
    }

    /// List all bookmarks visible to `remote`: rig-specific bookmarks merged
    /// with general bookmarks (rig-specific wins on duplicate IDs).
    pub fn list_for_rig(&self, remote: &str) -> Vec<Bookmark> {
        let mut map: HashMap<String, Bookmark> = self
            .general
            .list()
            .into_iter()
            .map(|bm| (bm.id.clone(), bm))
            .collect();
        for bm in self.store_for(remote).list() {
            map.insert(bm.id.clone(), bm);
        }
        map.into_values().collect()
    }
}
