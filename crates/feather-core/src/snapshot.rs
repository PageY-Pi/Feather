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
use std::path::{Path, PathBuf};
use std::sync::Arc;

const CHUNKS: TableDefinition<'static, u64, &[u8]> = TableDefinition::new("chunks");
const SNAPSHOTS: TableDefinition<'static, u64, &[u8]> = TableDefinition::new("snapshots");
/// Durable "which snapshot is the live context" pointer. `latest_turn()` is
/// the newest snapshot ever written; after a rewind the newest snapshot can be
/// *ahead* of the context the user rewound to, so resume must read the pointer,
/// not the maximum turn.
const META: TableDefinition<'static, &str, u64> = TableDefinition::new("meta");
const CURRENT_TURN_KEY: &str = "current_turn";

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

#[derive(Clone)]
pub struct SnapshotStore {
    db: Arc<Database>,
    path: PathBuf,
}

impl SnapshotStore {
    /// Open (creating if needed) `<dir>/snapshots.redb`.
    pub fn open(dir: &Path) -> Result<Self, SnapshotError> {
        std::fs::create_dir_all(dir)?;
        let path = dir.join("snapshots.redb");
        // ensure tables exist (redundant-but-cheap first commit)
        let db = crate::database::create(&path)?;
        let txn = db.begin_write()?;
        {
            txn.open_table(CHUNKS)?;
            txn.open_table(SNAPSHOTS)?;
            txn.open_table(META)?;
        }
        txn.commit()?;
        Ok(Self {
            db: Arc::new(db),
            path,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Persist a snapshot for `turn`. Idempotent: re-snapshotting the same
    /// content is a no-op; divergent content for the same turn overwrites.
    pub fn snapshot(&self, turn: u64, messages: &[Message]) -> Result<(), SnapshotError> {
        let mut keys = Vec::with_capacity(messages.len());
        // serialize each message once — the bytes feed the FNV key, the chunk
        // table, and the running history hash (was: 3 separate to_vec calls)
        let mut hist = crate::message::fnv1a(b"feather-history-v1");
        let txn = self.db.begin_write()?;
        {
            let mut chunks = txn.open_table(CHUNKS)?;
            for m in messages {
                let bytes = serde_json::to_vec(m)?;
                let base = crate::message::fnv1a(&bytes);
                hist = hist.rotate_left(1) ^ crate::message::fnv1a(&bytes);
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
        Ok(())
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
