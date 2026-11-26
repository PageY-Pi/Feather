//! Turn-granularity snapshot store on redb.
//!
//! feather's sessions are **linear** (a deliberate divergence from pi's
//! tree): after every completed turn — user prompt, assistant response(s),
//! all tool results — the agent snapshots the full message list. Storage is
//! content-addressed: each unique message is stored once as a *chunk* keyed
//! by FNV-1a-64 of its canonical JSON, and a snapshot is just the ordered
//! list of chunk ids. Rollback is O(turns) lookup, not O(messages) replay.
//!
//! This also gives crash-resumability: the latest snapshot always describes
//! a fully committed turn; a crash mid-turn loses at most the turn in
//! flight.

use crate::message::Message;
use redb::{Database, ReadableTable, TableDefinition};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, Weak};

const CHUNKS: TableDefinition<'static, u64, &[u8]> = TableDefinition::new("chunks");
const SNAPSHOTS: TableDefinition<'static, u64, &[u8]> = TableDefinition::new("snapshots");
/// Durable "which snapshot is the live context" pointer. `latest_turn()` is
/// the newest snapshot ever written; after a rewind the newest snapshot can be
/// *ahead* of the context the user rewound to, so resume must read the pointer,
/// not the maximum turn.
const META: TableDefinition<'static, &str, u64> = TableDefinition::new("meta");
const CURRENT_TURN_KEY: &str = "current_turn";
/// Durable turn index of every user instruction, oldest first.
///
/// Rewind is bounded in *instructions*, but a snapshot turn is one model round trip
/// and one instruction spans dozens of them. The mapping has to survive a reopen, or
/// a resumed session would lose its horizon and start growing without bound again.
/// Keyed by position in the list, valued by snapshot turn, so appending is a keyed
/// insert rather than a whole-list rewrite.
const INSTRUCTION_TURNS: TableDefinition<'static, u64, u64> =
    TableDefinition::new("instruction_turns");
/// Number of messages `snapshot_reusing` had to serialize, returned to the caller.
///
/// The point of the reuse hint is that a commit costs the changed suffix, not the
/// history length, so this is what the cost test pins. It is returned rather than
/// read from a global so it stays exact under parallel tests.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SnapshotCost {
    /// Messages whose canonical JSON had to be produced and stored.
    pub serialized: u64,
    /// Messages whose key was reused from the hint without serializing.
    pub reused: u64,
}

/// What a snapshot commit produced: state to carry forward, and what it cost.
#[derive(Debug, Clone, Default)]
pub struct SnapshotCommit {
    /// State to pass as the `reuse` hint on the next commit.
    pub chunks: SnapshotChunks,
    /// Work this commit actually performed.
    pub cost: SnapshotCost,
}

#[derive(Debug, thiserror::Error)]
pub enum SnapshotError {
    #[error("redb error: {0}")]
    Db(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("table error: {0}")]
    Table(String),
}

impl From<redb::DatabaseError> for SnapshotError {
    fn from(e: redb::DatabaseError) -> Self {
        SnapshotError::Db(e.to_string())
    }
}

impl From<redb::TransactionError> for SnapshotError {
    fn from(e: redb::TransactionError) -> Self {
        SnapshotError::Db(e.to_string())
    }
}

impl From<redb::TableError> for SnapshotError {
    fn from(e: redb::TableError) -> Self {
        SnapshotError::Db(e.to_string())
    }
}

impl From<redb::StorageError> for SnapshotError {
    fn from(e: redb::StorageError) -> Self {
        SnapshotError::Db(e.to_string())
    }
}

impl From<redb::CommitError> for SnapshotError {
    fn from(e: redb::CommitError) -> Self {
        SnapshotError::Db(e.to_string())
    }
}

/// One snapshot: turn number -> ordered chunk ids.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct SnapshotRecord {
    turn: u64,
    /// 16-byte big-endian chunk keys.
    chunk_keys: Vec<u64>,
    /// FNV of the whole message list — used by the cache-optimizer as the
    /// "history generation" marker.
    history_hash: u64,
}

/// The chunk keys a snapshot commit wrote, paired with the message hashes they
/// were derived from.
///
/// Held by `Session` between commits so the next snapshot can reuse the unchanged
/// prefix instead of re-serializing the whole conversation. It is a cache, not
/// state: `snapshot_reusing` re-checks every entry by hash, so a stale or empty
/// value can only cost extra work, never a wrong snapshot.
#[derive(Debug, Clone, Default)]
pub struct SnapshotChunks {
    /// Chunk key recorded for each message, in message order.
    pub keys: Vec<u64>,
    /// FNV-1a of each message's canonical JSON at the time it was stored.
    pub bases: Vec<u64>,
}

impl SnapshotChunks {
    /// Number of leading messages whose stored key is still valid for `hashes`.
    fn reusable_prefix(&self, hashes: &[u64]) -> usize {
        self.bases
            .iter()
            .zip(hashes.iter())
            .take_while(|(stored, current)| stored == current)
            .count()
    }
}

/// Process-wide `Weak` handles to open snapshot databases.
///
/// A `Weak` (not `Arc`) so a session that is dropped actually releases its
/// file handle: the cache must never keep a database alive on its own, or a
/// closed session would still block deletion on Windows.
fn snapshot_cache() -> &'static Mutex<BTreeMap<PathBuf, Weak<Database>>> {
    static CACHE: OnceLock<Mutex<BTreeMap<PathBuf, Weak<Database>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(BTreeMap::new()))
}
#[derive(Clone)]
pub struct SnapshotStore {
    db: Arc<Database>,
    path: PathBuf,
}

impl SnapshotStore {
    /// Open (creating if needed) `<dir>/snapshots.redb`.
    ///
    /// The handle is shared process-wide through a `Weak` cache, like
    /// `session_store`/`records`. redb permits exactly one open handle per file
    /// per process, so a live `Session` (which keeps this store) used to make
    /// any second `open` of the same file fail with "database already open" —
    /// a rewind-menu refresh, a test, or a second view of the same session.
    /// Reusing the live handle removes the failure rather than retrying it.
    pub fn open(dir: &Path) -> Result<Self, SnapshotError> {
        std::fs::create_dir_all(dir)?;
        let path = std::fs::canonicalize(dir)?.join("snapshots.redb");
        if let Some(db) = snapshot_cache()
            .lock()
            .map_err(|e| SnapshotError::Db(e.to_string()))?
            .get(&path)
            .and_then(Weak::upgrade)
        {
            return Ok(Self { db, path });
        }
        // ensure tables exist (redundant-but-cheap first commit)
        let db = Arc::new(crate::database::create(&path)?);
        let txn = db.begin_write()?;
        {
            txn.open_table(CHUNKS)?;
            txn.open_table(SNAPSHOTS)?;
            txn.open_table(META)?;
            txn.open_table(INSTRUCTION_TURNS)?;
        }
        txn.commit()?;
        snapshot_cache()
            .lock()
            .map_err(|e| SnapshotError::Db(e.to_string()))?
            .insert(path.clone(), Arc::downgrade(&db));
        Ok(Self { db, path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Persist a snapshot for `turn`. Idempotent: re-snapshotting the same
    /// content is a no-op; divergent content for the same turn overwrites.
    ///
    /// Cost is proportional to what *changed*, not to the length of the history.
    /// Sessions are append-only between compactions, so the newest snapshot is
    /// almost always a prefix of the new message list; reusing its chunk keys for
    /// that prefix avoids re-serializing and re-hashing the entire conversation
    /// on every tool-loop iteration (which made a long session quadratic in
    /// bytes written through `serde_json`).
    pub fn snapshot(&self, turn: u64, messages: &[Message]) -> Result<(), SnapshotError> {
        self.snapshot_reusing(turn, messages, &[], &SnapshotChunks::default())
            .map(|_| ())
    }

    /// Snapshot reusing known chunk keys for an unchanged prefix.
    ///
    /// `message_hashes[i]` must be FNV-1a of `messages[i]`'s canonical JSON (the
    /// chunk key domain). `reuse` carries what a previous commit stored; an entry
    /// is taken only when its recorded hash still matches, so a stale or short
    /// hint simply falls back to storing that chunk. The returned value describes
    /// what this commit wrote, ready to pass back on the next call.
    pub fn snapshot_reusing(
        &self,
        turn: u64,
        messages: &[Message],
        message_hashes: &[u64],
        reuse: &SnapshotChunks,
    ) -> Result<SnapshotCommit, SnapshotError> {
        let mut keys = Vec::with_capacity(messages.len());
        let mut cost = SnapshotCost::default();
        let mut base_hashes: Vec<u64> = Vec::with_capacity(messages.len());
        let mut hist = crate::message::fnv1a(b"feather-history-v1");
        let txn = self.db.begin_write()?;
        {
            let mut chunks = txn.open_table(CHUNKS)?;
            // Reuse the prefix this turn did not change. `message_hashes` is the
            // same FNV-1a of each message's canonical JSON that keys the chunk
            // table, maintained incrementally by `Session::push`, so an equal
            // hash at position i proves the bytes are the ones already stored —
            // no serialization, no read-back, and the collision probe resolved
            // exactly as it was before.
            // Messages that still hash to what was stored need no serialization
            // and no chunk-table read: their key is already correct.
            let reusable = reuse.reusable_prefix(message_hashes);
            for (index, m) in messages.iter().enumerate() {
                let base = match message_hashes.get(index) {
                    Some(hash) => *hash,
                    None => crate::message::json_hash(m)?,
                };
                base_hashes.push(base);
                hist = hist.rotate_left(1) ^ base;
                if index < reusable {
                    keys.push(reuse.keys[index]);
                    cost.reused += 1;
                    continue;
                }
                cost.serialized += 1;
                let bytes = serde_json::to_vec(m)?;
                // FNV-64 collision guard: a key hit with DIFFERENT bytes must
                // not silently alias the other message — probe neighbor keys
                let mut key = base;
                let mut probe: u64 = 0;
                loop {
                    // read first, drop the guard, then insert (redb borrows)
                    let hit = chunks.get(key)?.map(|g| g.value() == bytes);
                    match hit {
                        None => {
                            chunks.insert(key, bytes.as_slice())?;
                            break;
                        }
                        Some(true) => break, // same content
                        Some(false) => {
                            probe += 1;
                            key = base ^ probe;
                        }
                    }
                }
                keys.push(key);
            }
        }
        // Kept for the caller so the next commit can reuse what this one stored.
        let chunk_keys = keys.clone();
        {
            let mut snaps = txn.open_table(SNAPSHOTS)?;
            let rec = SnapshotRecord {
                turn,
                chunk_keys: keys,
                // running hash accumulated while serializing chunks — identical
                // to history_hash() without a second full serialization pass
                history_hash: hist,
            };
            snaps.insert(turn, serde_json::to_vec(&rec)?.as_slice())?;
            // A snapshot defines a *linear* timeline: anything recorded after
            // this turn is a divergent future and must not survive a resume
            // (otherwise `/rewind` + a new prompt would resurrect stale turns).
            let stale: Vec<u64> = snaps
                .range((turn + 1)..)?
                .filter_map(|row| row.ok().map(|(k, _)| k.value()))
                .collect();
            for k in stale {
                snaps.remove(k)?;
            }
        }
        {
            let mut meta = txn.open_table(META)?;
            meta.insert(CURRENT_TURN_KEY, turn)?;
        }
        txn.commit()?;
        Ok(SnapshotCommit {
            chunks: SnapshotChunks {
                keys: chunk_keys,
                bases: base_hashes,
            },
            cost,
        })
    }

    pub fn history_hash(messages: &[Message]) -> u64 {
        let mut h = crate::message::fnv1a(b"feather-history-v1");
        for m in messages {
            let message_hash =
                crate::message::json_hash(m).unwrap_or_else(|_| crate::message::fnv1a(&[]));
            h = h.rotate_left(1) ^ message_hash;
        }
        h
    }

    /// Load the message list captured at `turn`.
    pub fn load(&self, turn: u64) -> Result<Vec<Message>, SnapshotError> {
        let txn = self.db.begin_read()?;
        let snaps = txn.open_table(SNAPSHOTS)?;
        let rec_bytes = snaps
            .get(turn)?
            .ok_or_else(|| SnapshotError::Table(format!("no snapshot for turn {turn}")))?
            .value()
            .to_vec();
        let rec: SnapshotRecord = serde_json::from_slice(&rec_bytes)?;
        let chunks = txn.open_table(CHUNKS)?;
        let mut out = Vec::with_capacity(rec.chunk_keys.len());
        for key in &rec.chunk_keys {
            let bytes = chunks
                .get(*key)?
                .ok_or_else(|| SnapshotError::Table(format!("missing chunk {key:016x}")))?
                .value()
                .to_vec();
            out.push(serde_json::from_slice(&bytes)?);
        }
        Ok(out)
    }

    /// Bounded lightweight rewind labels. Decode only the newest real user
    /// request in each snapshot, never load whole snapshot message vectors.
    pub fn turn_labels(&self, limit: usize) -> Result<Vec<(u64, String)>, SnapshotError> {
        let tx = self.db.begin_read()?;
        let snaps = tx.open_table(SNAPSHOTS)?;
        let chunks = tx.open_table(CHUNKS)?;
        let mut labels = Vec::new();
        for entry in snaps.range::<u64>(..)?.rev().take(limit.min(256)) {
            let (turn, bytes) = entry?;
            let rec: SnapshotRecord = serde_json::from_slice(bytes.value())?;
            let mut label = None;
            for key in rec.chunk_keys.iter().rev() {
                if let Some(raw) = chunks.get(*key)? {
                    let message: Message = serde_json::from_slice(raw.value())?;
                    if message.role == crate::message::Role::User {
                        let text = message.text();
                        if !text.starts_with("<feather-runtime-diagnostic>")
                            && !text.starts_with("## Conversation summary")
                        {
                            label = Some(text.chars().take(180).collect::<String>());
                            break;
                        }
                    }
                }
            }
            labels.push((
                turn.value(),
                label.unwrap_or_else(|| "Context checkpoint".into()),
            ));
        }
        Ok(labels)
    }
    /// Highest turn number with a snapshot (0 when fresh).
    pub fn latest_turn(&self) -> Result<u64, SnapshotError> {
        let txn = self.db.begin_read()?;
        let snaps = txn.open_table(SNAPSHOTS)?;
        Ok(snaps
            .range::<u64>(..)?
            .next_back()
            .and_then(|row| row.ok())
            .map(|(k, _)| k.value())
            .unwrap_or(0))
    }

    /// The durable "live context" turn. After `/rewind` this points at the
    /// rewound-to turn even though newer snapshots still exist, so reopening a
    /// session restores the current context and not a stale future turn.
    /// Falls back to `latest_turn()` for stores written before the pointer
    /// existed.
    /// Turns at which the surviving user instructions began, oldest first.
    ///
    /// Persisted so a reopened session keeps its rewind horizon. Without this a resume
    /// starts with an empty list and the store grows unbounded again — the bug this
    /// whole mechanism exists to fix, reintroduced silently on the resume path.
    pub fn instruction_turns(&self) -> Result<Vec<u64>, SnapshotError> {
        let txn = self.db.begin_read()?;
        let table = txn.open_table(INSTRUCTION_TURNS)?;
        let mut out = Vec::new();
        for row in table.range::<u64>(..)? {
            let (_, v) = row?;
            out.push(v.value());
        }
        Ok(out)
    }

    /// Replace the persisted instruction boundary list.
    ///
    /// Truncating and rewriting rather than diffing: the list is at most a few dozen
    /// entries, and a wholesale write cannot leave a half-updated boundary list behind
    /// if the process dies mid-call.
    pub fn set_instruction_turns(&self, turns: &[u64]) -> Result<(), SnapshotError> {
        let txn = self.db.begin_write()?;
        {
            let mut table = txn.open_table(INSTRUCTION_TURNS)?;
            let existing: Vec<u64> = table
                .range::<u64>(..)?
                .filter_map(|row| row.ok().map(|(k, _)| k.value()))
                .collect();
            for key in existing {
                table.remove(key)?;
            }
            for (index, turn) in turns.iter().enumerate() {
                table.insert(index as u64, *turn)?;
            }
        }
        txn.commit()?;
        Ok(())
    }

    pub fn current_turn(&self) -> Result<u64, SnapshotError> {
        let txn = self.db.begin_read()?;
        let meta = txn.open_table(META)?;
        if let Some(v) = meta.get(CURRENT_TURN_KEY)? {
            return Ok(v.value());
        }
        let snaps = txn.open_table(SNAPSHOTS)?;
        Ok(snaps
            .range::<u64>(..)?
            .next_back()
            .and_then(|row| row.ok())
            .map(|(k, _)| k.value())
            .unwrap_or(0))
    }

    /// Move the durable current-context pointer without deleting history.
    /// `/rewind` uses this so the restored turn survives a process restart
    /// (forward snapshots stay available until a divergent commit truncates
    /// them).
    pub fn set_current(&self, turn: u64) -> Result<(), SnapshotError> {
        self.set_current_with_context(turn, true)
    }
    /// Move the context pointer without deleting forward snapshots. A new
    /// prompt will truncate divergent future snapshots when it is committed.
    pub fn set_current_with_context(
        &self,
        turn: u64,
        _preserve_future: bool,
    ) -> Result<(), SnapshotError> {
        let txn = self.db.begin_write()?;
        {
            let snaps = txn.open_table(SNAPSHOTS)?;
            if snaps.get(turn)?.is_none() {
                return Err(SnapshotError::Table(format!("no snapshot for turn {turn}")));
            }
        }
        {
            let mut meta = txn.open_table(META)?;
            meta.insert(CURRENT_TURN_KEY, turn)?;
        }
        txn.commit()?;
        Ok(())
    }

    /// List `(turn, message_count, history_hash)` newest last.
    pub fn list(&self) -> Result<Vec<(u64, usize, u64)>, SnapshotError> {
        let txn = self.db.begin_read()?;
        let snaps = txn.open_table(SNAPSHOTS)?;
        let mut out = Vec::new();
        for row in snaps.range::<u64>(..)? {
            let (_, v) = row?;
            let rec: SnapshotRecord = serde_json::from_slice(v.value())?;
            out.push((rec.turn, rec.chunk_keys.len(), rec.history_hash));
        }
        Ok(out)
    }

    /// Count stored chunks and how many of them any snapshot still references.
    ///
    /// The gap is what [`Self::vacuum`] would delete. Exposed because that gap was
    /// invisible: `vacuum` had no caller outside a test, so a session's store only
    /// ever grew, and there was no way to see by how much without rewriting the
    /// database. Returns `(stored, referenced)`.
    pub fn chunk_census(&self) -> Result<(usize, usize), SnapshotError> {
        let rd = self.db.begin_read()?;
        let stored = {
            let chunks = rd.open_table(CHUNKS)?;
            chunks.iter()?.filter(|row| row.is_ok()).count()
        };
        let referenced = {
            let snaps = rd.open_table(SNAPSHOTS)?;
            let mut set = std::collections::HashSet::new();
            for row in snaps.range::<u64>(..)? {
                let (_, v) = row?;
                let rec: SnapshotRecord = serde_json::from_slice(v.value())?;
                set.extend(rec.chunk_keys);
            }
            set.len()
        };
        Ok((stored, referenced))
    }

    /// Drop every snapshot older than `keep_from_turn`.
    ///
    /// A snapshot turn is one model round trip, so "keep the newest N turns" is the
    /// wrong knob — it was tried and measured wrong. One user instruction spans dozens
    /// of turns, so a turn horizon keeps a sliver of a single instruction and throws
    /// away the window the user was promised. There is deliberately no `prune_to_last`
    /// for that reason: the only supported unit is an instruction.
    /// `Session` records the turn each instruction starts on and passes that floor
    /// here, which cuts on a real instruction boundary rather than mid-loop.
    ///
    /// The live turn is always kept even when it sits outside the window: resume
    /// reads that pointer, so pruning it would corrupt the session.
    pub fn prune_before(&self, keep_from_turn: u64) -> Result<usize, SnapshotError> {
        let live = self.current_turn()?;
        let doomed: Vec<u64> = {
            let rd = self.db.begin_read()?;
            let snaps = rd.open_table(SNAPSHOTS)?;
            snaps
                .range::<u64>(..keep_from_turn)?
                .filter_map(|row| row.ok().map(|(k, _)| k.value()))
                .filter(|turn| *turn != live)
                .collect()
        };
        if doomed.is_empty() {
            return Ok(0);
        }
        let txn = self.db.begin_write()?;
        {
            let mut snaps = txn.open_table(SNAPSHOTS)?;
            for turn in &doomed {
                snaps.remove(*turn)?;
            }
        }
        txn.commit()?;
        // Freed pages are not disk until the chunks go and the file is compacted;
        // see `compact_file`. Vacuum here, compaction is the caller's step because
        // it needs an exclusive handle.
        self.vacuum()?;
        Ok(doomed.len())
    }

    /// Return freed pages to the filesystem.
    ///
    /// `prune_before` removes rows and `vacuum` drops the now-unreferenced
    /// chunks, but redb keeps the file's high-water mark: measured on a real
    /// 2372-turn session, pruning to 10 turns took the chunk count from 4849 to
    /// 751 while the file stayed at 96.9 MiB. Compaction brought it to 7.7 MiB.
    /// Without this step pruning is bookkeeping only.
    ///
    /// This needs a *unique* handle — `redb` returns `TransactionInProgress`
    /// otherwise — and the process-wide `Weak` cache plus any live `Session`
    /// clone both hold one. So the caller drops its store first and passes the
    /// path; the cache is cleared so a later `open` rebuilds from the compacted
    /// file rather than a stale handle.
    pub fn compact_file(path: &Path) -> Result<bool, SnapshotError> {
        snapshot_cache()
            .lock()
            .map_err(|e| SnapshotError::Db(e.to_string()))?
            // Same key shape `open` inserts: canonical directory + file name.
            .remove(&path.canonicalize().unwrap_or_else(|_| path.to_path_buf()));
        let mut db = crate::database::open(path)?;
        match db.compact() {
            Ok(did) => Ok(did),
            // Losing the shrink is not worth failing a turn over: the rows are
            // already gone, so the session is correct and only the file is fat.
            // It gets another chance next time the horizon is crossed.
            Err(redb::CompactionError::TransactionInProgress) => Ok(false),
            Err(e) => Err(SnapshotError::Db(e.to_string())),
        }
    }

    /// Best-effort compaction of chunks not referenced by any snapshot.
    pub fn vacuum(&self) -> Result<usize, SnapshotError> {
        let txn = self.db.begin_write()?;
        let mut removed = 0usize;
        {
            let mut chunks = txn.open_table(CHUNKS)?;
            let referenced: std::collections::HashSet<u64> = {
                let rd = self.db.begin_read()?;
                let snaps = rd.open_table(SNAPSHOTS)?;
                let mut set = std::collections::HashSet::new();
                for row in snaps.range::<u64>(..)? {
                    let (_, v) = row?;
                    let rec: SnapshotRecord = serde_json::from_slice(v.value())?;
                    set.extend(rec.chunk_keys);
                }
                set
            };
            let dead: Vec<u64> = chunks
                .range::<u64>(..)?
                .filter_map(|row| {
                    let (k, _) = row.ok()?;
                    let k = k.value();
                    (!referenced.contains(&k)).then_some(k)
                })
                .collect();
            for k in dead {
                let _ = chunks.remove(k);
                removed += 1;
            }
        }
        txn.commit()?;
        Ok(removed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::{Message, StopReason, Usage};

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "feather-snap-{tag}-{}-{}",
            std::process::id(),
            crate::message::now_millis()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// Opening the same session's snapshot store twice must succeed. redb allows
    /// one handle per file per process, so without the shared cache the second
    /// open fails with "database already open" — the failure that forced the
    /// GUI to retry session reopening in a loop.
    #[test]
    fn opening_the_same_store_twice_reuses_one_handle() {
        let dir = tmpdir("reopen");
        let first = SnapshotStore::open(&dir).unwrap();
        let second = SnapshotStore::open(&dir).unwrap();

        // Same underlying database, so a write through one is visible to the
        // other without either closing.
        first.snapshot(1, &[Message::user_text("shared")]).unwrap();
        assert_eq!(second.latest_turn().unwrap(), 1);
        assert_eq!(second.load(1).unwrap().len(), 1);
        // Both clone cheaply off the shared handle.
        let _third = first.clone();
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Reusing a prefix must be invisible in the result: a snapshot written with
    /// a hint has to load back byte-identically, and its recorded history hash
    /// has to equal a full recomputation. If reuse ever diverged, rewind and
    /// resume would restore a different conversation from the one committed.
    #[test]
    fn hinted_snapshot_matches_a_full_rebuild() {
        let dir = tmpdir("reuse");
        let store = SnapshotStore::open(&dir).unwrap();

        let base = vec![
            Message::user_text("first"),
            Message::assistant(
                vec![crate::message::ContentBlock::Text {
                    text: "answer".into(),
                }],
                "m",
                crate::message::Api::Faux,
                Usage::default(),
                StopReason::Stop,
            ),
        ];
        let hashes: Vec<u64> = base
            .iter()
            .map(|m| crate::message::json_hash(m).unwrap())
            .collect();

        store.snapshot(1, &base).unwrap();
        let first = store.list().unwrap();
        assert_eq!(first.len(), 1);

        // Grow the history and reuse the prefix stored by the first commit.
        let mut grown = base.clone();
        grown.push(Message::user_text("second"));
        let mut grown_hashes = hashes.clone();
        grown_hashes.push(crate::message::json_hash(&grown[2]).unwrap());

        let reuse = SnapshotChunks {
            keys: vec![],
            bases: vec![],
        };
        let stored = store
            .snapshot_reusing(2, &grown, &grown_hashes, &reuse)
            .unwrap();
        assert_eq!(stored.chunks.bases, grown_hashes);

        // A second commit that actually reuses the hint.
        let mut grown2 = grown.clone();
        grown2.push(Message::user_text("third"));
        let mut hashes2 = grown_hashes.clone();
        hashes2.push(crate::message::json_hash(&grown2[3]).unwrap());
        let stored2 = store
            .snapshot_reusing(3, &grown2, &hashes2, &stored.chunks)
            .unwrap();
        assert_eq!(stored2.chunks.bases, hashes2);
        // Only the new message needed storing; the prefix came from the hint.
        assert_eq!(stored2.cost.serialized, 1);
        assert_eq!(stored2.cost.reused, grown.len() as u64);

        // Every turn loads back to exactly what was passed in.
        assert_eq!(store.load(1).unwrap(), base);
        assert_eq!(store.load(2).unwrap(), grown);
        assert_eq!(store.load(3).unwrap(), grown2);

        // The recorded history hash is the same one a full pass computes.
        let listed = store.list().unwrap();
        let hash_at = |turn: u64| listed.iter().find(|t| t.0 == turn).unwrap().2;
        assert_eq!(hash_at(3), SnapshotStore::history_hash(&grown2));

        let _ = std::fs::remove_dir_all(dir);
    }

    /// A hint that no longer matches the history must degrade to storing the
    /// changed chunks, never to recording the old keys.
    #[test]
    fn stale_hint_falls_back_to_storing_chunks() {
        let dir = tmpdir("reuse-stale");
        let store = SnapshotStore::open(&dir).unwrap();

        let original = vec![Message::user_text("a"), Message::user_text("b")];
        store.snapshot(1, &original).unwrap();

        let rewritten = vec![Message::user_text("a"), Message::user_text("b")];
        let mut hashes: Vec<u64> = rewritten
            .iter()
            .map(|m| crate::message::json_hash(m).unwrap())
            .collect();
        // A hint claiming different bases: nothing may be reused from it.
        let stale = SnapshotChunks {
            keys: vec![0xdead, 0xbeef],
            bases: vec![1, 2],
        };
        let stored = store
            .snapshot_reusing(2, &rewritten, &hashes, &stale)
            .unwrap();
        assert_eq!(stored.chunks.bases, hashes, "hint bases must be recomputed");
        assert_ne!(
            stored.chunks.keys,
            vec![0xdead, 0xbeef],
            "stale keys were reused"
        );
        assert_eq!(stored.cost.reused, 0, "nothing should have been reused");
        assert_eq!(stored.cost.serialized, 2);
        assert_eq!(store.load(2).unwrap(), rewritten);

        // A short history with a long hint is also safe.
        hashes.truncate(1);
        let short = &rewritten[..1];
        let stored_short = store.snapshot_reusing(3, short, &hashes, &stale).unwrap();
        assert_eq!(store.load(3).unwrap(), short);
        assert_eq!(stored_short.chunks.bases.len(), 1);
        assert_eq!(stored_short.cost.reused, 0);

        let _ = std::fs::remove_dir_all(dir);
    }

    /// The reuse hint has to actually skip work: commit cost must track the
    /// changed suffix, not the length of the history. A test that only checked
    /// `load()` equality would pass even if every message were re-serialized.
    #[test]
    fn reuse_serializes_only_the_changed_suffix() {
        let dir = tmpdir("reuse-cost");
        let store = SnapshotStore::open(&dir).unwrap();

        let mut messages: Vec<Message> = (0..50)
            .map(|n| Message::user_text(format!("message {n}")))
            .collect();
        let hashes = |msgs: &[Message]| -> Vec<u64> {
            msgs.iter()
                .map(|m| crate::message::json_hash(m).unwrap())
                .collect()
        };

        let mut hint = store
            .snapshot_reusing(1, &messages, &hashes(&messages), &SnapshotChunks::default())
            .unwrap()
            .chunks;

        // Each new turn appends one message. Only that message may be stored.
        for turn in 2..=6u64 {
            messages.push(Message::user_text(format!("turn {turn}")));
            let commit = store
                .snapshot_reusing(turn, &messages, &hashes(&messages), &hint)
                .unwrap();
            hint = commit.chunks;
            assert_eq!(
                commit.cost.serialized, 1,
                "turn {turn} re-serialized the whole history instead of the suffix"
            );
            assert_eq!(commit.cost.reused as usize, messages.len() - 1);
            assert_eq!(store.load(turn).unwrap(), messages);
        }

        // A prefix rewrite (what compaction does) must invalidate from the edit
        // onward — the reused prefix simply ends there.
        messages[3] = Message::user_text("rewritten");
        let commit = store
            .snapshot_reusing(7, &messages, &hashes(&messages), &hint)
            .unwrap();
        assert_eq!(
            commit.cost.serialized,
            (messages.len() - 3) as u64,
            "rewrite from index 3 onward"
        );
        assert_eq!(commit.cost.reused, 3);
        assert_eq!(store.load(7).unwrap(), messages);
    }
    #[test]
    fn roundtrip_and_rollback() {
        let dir = tmpdir("rt");
        let store = SnapshotStore::open(&dir).unwrap();

        let m1 = vec![Message::user_text("hello")];
        store.snapshot(1, &m1).unwrap();

        let m2 = vec![
            Message::user_text("hello"),
            Message::assistant(
                vec![crate::message::ContentBlock::Text { text: "hi!".into() }],
                "test-model",
                crate::message::Api::Faux,
                Usage::default(),
                StopReason::Stop,
            ),
        ];
        store.snapshot(2, &m2).unwrap();

        assert_eq!(store.latest_turn().unwrap(), 2);
        assert_eq!(store.load(1).unwrap(), m1);
        assert_eq!(store.load(2).unwrap(), m2);
        // rollback semantics: loading turn 1 and snapshotting it as turn 3
        store.snapshot(3, &m1).unwrap();
        assert_eq!(store.load(3).unwrap(), m1);
        // shared chunk is deduped: m1 stored once
        let list = store.list().unwrap();
        assert_eq!(list.len(), 3);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn vacuum_drops_unreferenced_chunks() {
        let dir = tmpdir("vac");
        let store = SnapshotStore::open(&dir).unwrap();
        let a = vec![Message::user_text("a")];
        store.snapshot(1, &a).unwrap();
        // turn 1 replaced by different content: old "a" chunk becomes garbage
        let b = vec![Message::user_text("b")];
        store.snapshot(1, &b).unwrap();
        assert_eq!(store.load(1).unwrap(), b);
        let removed = store.vacuum().unwrap();
        assert!(removed >= 1);
        assert_eq!(store.load(1).unwrap(), b);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Pruning keeps the newest `keep` turns and drops the rest.
    #[test]
    fn prune_keeps_the_newest_turns_and_drops_the_rest() {
        let dir = tmpdir("prune");
        let store = SnapshotStore::open(&dir).unwrap();
        for turn in 1..=20u64 {
            store
                .snapshot(turn, &[Message::user_text(format!("turn {turn}"))])
                .unwrap();
        }
        assert_eq!(store.list().unwrap().len(), 20);
        let dropped = store.prune_before(11).unwrap();
        assert_eq!(dropped, 10, "half the turns should go");
        let left: Vec<u64> = store.list().unwrap().iter().map(|(t, _, _)| *t).collect();
        assert_eq!(left, (11..=20).collect::<Vec<u64>>(), "newest ten survive");
        // The surviving turns must still load, or pruning destroyed rewind.
        // Compared by text: `user_text` stamps `now_millis()`, so two structurally
        // identical messages are never `==`.
        let loaded = store.load(20).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].text(), "turn 20");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A rewind leaves the live pointer *behind* the newest snapshot. Pruning must
    /// honour the pointer, or resuming after a rewind loads a turn that is gone and
    /// the session is corrupt.
    #[test]
    fn prune_never_drops_the_turn_the_session_is_actually_on() {
        let dir = tmpdir("prunelive");
        let store = SnapshotStore::open(&dir).unwrap();
        for turn in 1..=20u64 {
            store
                .snapshot(turn, &[Message::user_text(format!("turn {turn}"))])
                .unwrap();
        }
        // Rewound to turn 3, which is far outside a ten-turn horizon.
        store.set_current(3).unwrap();
        store.prune_before(11).unwrap();
        let left: Vec<u64> = store.list().unwrap().iter().map(|(t, _, _)| *t).collect();
        assert!(
            left.contains(&3),
            "the live turn must survive pruning, left={left:?}"
        );
        assert_eq!(store.current_turn().unwrap(), 3);
        let live = store.load(3).unwrap();
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].text(), "turn 3", "and it must still be readable");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Pruning also drops the chunks only the pruned turns referenced — that is
    /// where the bytes are. A chunk shared with a survivor must stay.
    #[test]
    fn prune_vacates_chunks_only_the_dropped_turns_referenced() {
        let dir = tmpdir("prunechunks");
        let store = SnapshotStore::open(&dir).unwrap();
        // The shape that makes chunks reclaimable is the one compaction produces:
        // an old turn's message list contains text that the newest list no longer
        // has. A growing *prefix* would keep every chunk reachable from the newest
        // turn and prove nothing — measured 21 -> 21 when this test first did that.
        let mut shared = Message::user_text("shared preamble");
        for turn in 1..=20u64 {
            shared.timestamp = 0;
            store
                .snapshot(
                    turn,
                    &[
                        shared.clone(),
                        Message::user_text(format!("only turn {turn}")),
                    ],
                )
                .unwrap();
        }
        let (stored_before, _) = store.chunk_census().unwrap();
        let dropped = store.prune_before(11).unwrap();
        assert_eq!(dropped, 10);
        let (stored_after, referenced) = store.chunk_census().unwrap();
        assert!(
            stored_after < stored_before,
            "chunks only old turns used must go: {stored_before} -> {stored_after}"
        );
        assert_eq!(stored_after, referenced, "nothing unreferenced may remain");
        // The shared preamble is referenced by every survivor, so it stays.
        let survivor = store.load(20).unwrap();
        assert_eq!(survivor.first().unwrap().text(), "shared preamble");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `chunk_census` must agree with `vacuum`: the gap it reports is exactly what
    /// vacuum deletes. Reporting a gap that vacuum then leaves behind would make
    /// the measurement in `examples/snapshot_gc.rs` a lie.
    #[test]
    fn census_reports_exactly_what_vacuum_would_remove() {
        let dir = tmpdir("census");
        let store = SnapshotStore::open(&dir).unwrap();
        let a = vec![Message::user_text("a")];
        store.snapshot(1, &a).unwrap();
        // Same turn overwritten: the "a" chunk is now unreferenced.
        let b = vec![Message::user_text("b")];
        store.snapshot(1, &b).unwrap();

        let (stored, referenced) = store.chunk_census().unwrap();
        assert_eq!(
            stored - referenced,
            1,
            "one chunk should be unreferenced after the overwrite"
        );
        let removed = store.vacuum().unwrap();
        assert_eq!(
            removed,
            stored - referenced,
            "vacuum must remove exactly the gap the census reported"
        );
        let (stored_after, referenced_after) = store.chunk_census().unwrap();
        assert_eq!(stored_after, referenced_after, "nothing dead after vacuum");
        assert_eq!(store.load(1).unwrap(), b, "the live snapshot still loads");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn current_pointer_rewind_survives_reopen_and_truncates_divergence() {
        let dir = tmpdir("cur");
        let store = SnapshotStore::open(&dir).unwrap();
        let m1 = vec![Message::user_text("one")];
        let m2 = vec![Message::user_text("one"), Message::user_text("two")];
        let m3 = vec![
            Message::user_text("one"),
            Message::user_text("two"),
            Message::user_text("three"),
        ];
        store.snapshot(1, &m1).unwrap();
        store.snapshot(2, &m2).unwrap();
        store.snapshot(3, &m3).unwrap();
        assert_eq!(store.current_turn().unwrap(), 3);

        // rewind: keep history, move the durable pointer
        store.set_current(1).unwrap();
        assert_eq!(store.current_turn().unwrap(), 1);
        assert_eq!(store.latest_turn().unwrap(), 3, "forward history preserved");
        assert_eq!(store.load(1).unwrap(), m1);

        // reopen: the pointer, not latest_turn, drives resume
        drop(store);
        let store = SnapshotStore::open(&dir).unwrap();
        assert_eq!(store.current_turn().unwrap(), 1);
        assert_eq!(store.load(store.current_turn().unwrap()).unwrap(), m1);

        // a divergent commit truncates the discarded future
        let divergent = vec![Message::user_text("one"), Message::user_text("diverged")];
        store.snapshot(2, &divergent).unwrap();
        assert_eq!(store.current_turn().unwrap(), 2);
        assert_eq!(store.latest_turn().unwrap(), 2, "turn 3 discarded");
        assert!(store.load(3).is_err());
        assert_eq!(store.load(2).unwrap(), divergent);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn set_current_rejects_unknown_turn() {
        let dir = tmpdir("setcur");
        let store = SnapshotStore::open(&dir).unwrap();
        store.snapshot(1, &[Message::user_text("x")]).unwrap();
        assert!(store.set_current(5).is_err());
        assert_eq!(store.current_turn().unwrap(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
