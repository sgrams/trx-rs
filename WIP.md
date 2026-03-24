# WIP: Per-Rig Bookmarks, Scheduler, Decoders, and Map Rig Selector

## Goal

Make bookmarks and scheduler per-rig, filter decoders tab by current rig, and
use remote names in the map rig selector.

---

## 1. Per-Rig Bookmark Storage

**Current**: Single `~/.config/trx-rs/bookmarks.db` shared by all rigs.

**Target**: One file per rig: `~/.config/trx-rs/bookmark.{remote_name}.db`
(e.g. `bookmark.ft817.db`, `bookmark.lidzbark-vhf.db`).

### Changes

- **`bookmarks.rs`** — `BookmarkStore::default_path()` → `BookmarkStore::default_path(remote: &str)`.
  Replace the single static path with a method that embeds the remote name.
- **`server.rs`** — Instead of opening one `BookmarkStore`, maintain a
  `HashMap<String, Arc<BookmarkStore>>` keyed by rig id (lazily opened).
  Provide a helper `bookmark_store_for(remote: &str) -> Arc<BookmarkStore>`.
- **`api.rs` bookmark endpoints** — All bookmark REST endpoints gain a required
  `?remote=` query parameter. Route to the corresponding per-rig store.
  Return 400 if `remote` is missing or unknown.
- **`bookmarks.js`** — Pass the active rig id (`lastActiveRigId`) as `?remote=`
  on every fetch/POST/PUT/DELETE. Reload bookmarks on rig switch.
- **Scheduler integration** — `spawn_scheduler_task` and `BackgroundDecodeManager`
  already receive `bookmark_store`; update them to resolve the per-rig store
  from the rig id they're operating on.

### Migration

On startup, if the legacy `bookmarks.db` exists and no per-rig files exist yet,
copy it to every configured rig as a one-time migration, then rename the old
file to `bookmarks.db.migrated`.

---

## 2. Per-Rig Scheduler Storage

**Current**: Single `~/.config/trx-rs/scheduler.db` with keys `sch:{remote}`.

**Target**: One file per rig: `~/.config/trx-rs/scheduler.{remote_name}.db`.

### Changes

- **`scheduler.rs`** — `SchedulerStore::default_path()` →
  `SchedulerStore::default_path(remote: &str)`. Same pattern as bookmarks.
  Remove the `sch:` key prefix since each file is already scoped to one rig.
  Use a single key like `config` instead.
- **`server.rs`** — Maintain `HashMap<String, Arc<SchedulerStore>>` keyed by
  rig id (same lazy-open pattern).
- **`api.rs` scheduler endpoints** — Already take `{remote}` path param;
  route to the per-rig store instead of the single shared store.
- **`scheduler.js`** — No URL changes needed (already uses `{remote}` in path).
  Verify bookmark loading uses per-rig bookmarks.

### Migration

Same pattern: if legacy `scheduler.db` exists and no per-rig files exist,
extract each `sch:{remote}` entry into its own `scheduler.{remote}.db`, then
rename old file.

---

## 3. Decoders Tab — Filter by Current Rig

**Current**: Decode history and live SSE stream are global (all rigs mixed).

**Target**: Decoders tab shows only decodes from the currently selected rig.

### Changes

- **`decode.rs` / `audio.rs`** — Check if decoded messages already carry a
  `rig_id` field. If not, tag each decode event with the originating rig id
  when it enters the history ring buffer.
- **`api.rs` `/decode/history`** — Accept optional `?remote=` query param.
  When present, filter history to only that rig's decodes.
- **`api.rs` `/decode` SSE** — Accept optional `?remote=` query param.
  When present, filter live events to that rig.
- **`app.js`** — When connecting to `/decode` SSE and fetching `/decode/history`,
  append `?remote={lastActiveRigId}`. Reconnect/refetch on rig switch.
- **`decode-history-worker.js`** — No changes needed if server-side filtering
  is applied (worker just decompresses what it gets).

### Decode message tagging

If `Ft8Message`, `AprsPacket`, etc. in `trx-core/src/decode.rs` don't already
have a `rig_id` field, add an optional `rig_id: Option<String>` to each type.
The server fills this when the decoder produces output. The history collector
preserves it. Filtering is done at query time.

---

## 4. Map Rig Selector — Use Remote Names

**Current**: `updateMapRigFilter()` in `app.js` populates `#map-rig-filter`
options using `lastRigDisplayNames[id] || id`. The `display_name` comes from
`RemoteRigEntry.display_name` which is the manufacturer/model string.

**Target**: Use the remote name (the rig id from config, e.g. `ft817`,
`lidzbark-vhf`) as the option value (already done), and show the
`display_name` if available (already done). The user wants to ensure remote
names are used consistently — verify and fix any places that fall back to
manufacturer/model instead of the configured remote name.

### Changes

- **`app.js` `updateMapRigFilter()`** — Already correct: uses `id` (remote
  name) as value and `lastRigDisplayNames[id] || id` as label. Verify this
  uses the remote name, not some internal index.
- **`api.rs` `/rigs`** — Verify `remote` field is the config remote name
  (rig id), not a generated identifier. Should already be correct.
- If `display_name` is not set, fall back to remote name (not
  "manufacturer model").

---

## Implementation Order

1. Per-rig bookmark storage (Rust backend + JS frontend + migration)
2. Per-rig scheduler storage (Rust backend + migration)
3. Decoders tab rig filtering (decode message tagging + API filtering + JS)
4. Map rig selector remote name cleanup (JS)

---

## Files to Modify

### Rust
- `src/trx-client/trx-frontend/trx-frontend-http/src/bookmarks.rs`
- `src/trx-client/trx-frontend/trx-frontend-http/src/scheduler.rs`
- `src/trx-client/trx-frontend/trx-frontend-http/src/server.rs`
- `src/trx-client/trx-frontend/trx-frontend-http/src/api.rs`
- `src/trx-client/trx-frontend/trx-frontend-http/src/audio.rs`
- `src/trx-core/src/decode.rs` (if rig_id tagging needed)

### JavaScript
- `src/trx-client/trx-frontend/trx-frontend-http/assets/web/app.js`
- `src/trx-client/trx-frontend/trx-frontend-http/assets/web/plugins/bookmarks.js`
- `src/trx-client/trx-frontend/trx-frontend-http/assets/web/plugins/scheduler.js`
