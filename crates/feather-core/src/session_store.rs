//! Session raw audit history + metadata on redb (`<session_dir>/session.redb`).
//!
//! Replaces the append-only `session.jsonl` transcript. The raw history is the
//! *audit* record: it keeps every pushed message verbatim, including
//! pre-compaction originals and pre-shrink tool results, so nothing a model
//! context drops is ever lost. Model context is still rebuilt from the linear
//! list in [`crate::session::Session`].
//!
//! Legacy `session.jsonl` files are imported here: validated end to end, written
//! into redb in a single atomic transaction, verified by reading back, and only
//! then renamed to a `.legacy` backup so the live session directory holds no
//! JSONL any more.
//!
//! The database handle is shared process-wide through a `Weak` cache (like the
//! session catalog): opening the same session directory twice in one process
//! reuses the same `Database` instead of fighting the file lock, while the cache
//! never keeps an otherwise-unused database open.

use crate::message::Message;
use redb::{Database, ReadableTable, TableDefinition};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::ops::Bound;
use std::sync::{Arc, Mutex, OnceLock, Weak};

const META: TableDefinition<'static, &str, &[u8]> = TableDefinition::new("session_meta");
const RAW: TableDefinition<'static, u64, &[u8]> = TableDefinition::new("raw_messages");

const K_ID: &str = "id";
const K_CWD: &str = "cwd";
const K_CREATED_MS: &str = "created_ms";
const K_MODIFIED_MS: &str = "modified_ms";
const K_FIRST_PROMPT: &str = "first_prompt";
const K_RAW_COUNT: &str = "raw_count";

/// Hard row cap for one [`SessionStore::raw_page_before`] page, so a GUI reader
/// can never be handed an unbounded slice of audit history.
pub const RAW_PAGE_MAX_ROWS: usize = 256;

const LEGACY_TRANSCRIPT: &str = "session.jsonl";

#[derive(Debug, thiserror::Error)]
pub enum SessionStoreError {
    #[error("redb error: {0}")]
    Db(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("table error: {0}")]
    Table(String),
    #[error("legacy transcript invalid: {0}")]
    Legacy(String),
}

impl From<redb::DatabaseError> for SessionStoreError {
    fn from(e: redb::DatabaseError) -> Self {
        Self::Db(e.to_string())
    }
}
impl From<redb::TransactionError> for SessionStoreError {
    fn from(e: redb::TransactionError) -> Self {
        Self::Db(e.to_string())
    }
}
impl From<redb::TableError> for SessionStoreError {
    fn from(e: redb::TableError) -> Self {
        Self::Db(e.to_string())
    }
}
impl From<redb::StorageError> for SessionStoreError {
    fn from(e: redb::StorageError) -> Self {
        Self::Db(e.to_string())
    }
}
impl From<redb::CommitError> for SessionStoreError {
    fn from(e: redb::CommitError) -> Self {
        Self::Db(e.to_string())
    }
}

/// Durable session metadata: identity, working directory and discovery fields.
/// Kept inside `session.redb` so the catalog is a cache, never the source of
/// truth.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionMeta {
    pub id: String,
    pub cwd: PathBuf,
    pub created_ms: u64,
    pub modified_ms: u64,
    pub first_prompt: String,
}

/// One raw audit row: the pushed message plus its entry id and timestamp.
#[derive(Debug, Clone, PartialEq)]
pub struct RawEntry {
    pub seq: u64,
    pub id: String,
    pub timestamp: u64,
    pub message: Message,
}

/// One bounded page of the raw audit, walked newest-first from a cursor and
/// returned oldest-first.
///
/// The cursor contract mirrors paging a transcript from its bottom (the newest
/// row) upwards:
///
/// * `before` echoes the request; `None` starts at the newest row. A row is
///   only ever included when `seq < before` (when `before` is set);
/// * `entries` are ascending by `seq` and each row is complete and verbatim —
///   the raw audit is never rewritten, shrunk or summarized by a read;
/// * `next_before` is the exclusive cursor for the next, older page. It is
///   `Some(oldest seq of this page)` while older rows are known to exist and
///   `None` once the start of the audit was reached (or when nothing matched),
///   so a pager stops on `None` without an empty final round trip;
/// * `total` is `K_RAW_COUNT` read from the same snapshot transaction as the
///   rows, so pages and totals never disagree under a concurrent writer;
/// * `bytes` is the summed stored (JSON) size of this page's rows.
#[derive(Debug, Clone, PartialEq)]
pub struct RawPage {
    pub before: Option<u64>,
    pub entries: Vec<RawEntry>,
    pub next_before: Option<u64>,
    pub total: u64,
    pub bytes: usize,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct RawRecord {
    id: String,
    timestamp: u64,
    message: Message,
}
/// The write format is identical to RawRecord, but serialization borrows the
/// already-owned message instead of cloning its text/images/tool arguments.
#[derive(serde::Serialize)]
struct RawRecordRef<'a> { id: &'a str, timestamp: u64, message: &'a Message }

/// Outcome of a legacy JSONL migration.
#[derive(Debug, Clone)]
pub struct LegacyImport {
    pub meta: SessionMeta,
    pub entries: usize,
    /// True when the source `session.jsonl` was renamed to the `.legacy` backup.
    pub jsonl_retired: bool,
}

static DB_CACHE: OnceLock<Mutex<HashMap<PathBuf, Weak<Database>>>> = OnceLock::new();
static OPEN_LOCK: Mutex<()> = Mutex::new(());

fn cache() -> &'static Mutex<HashMap<PathBuf, Weak<Database>>> {
    DB_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Handle to one session's redb store.
///
/// `Clone` shares the same `Arc<Database>` (and the same process-wide open
/// handle) rather than reopening the file, so a background reader — the GUI
/// paging the raw audit — can hold its own clone while the owning agent keeps
/// appending through its handle. redb serializes the two, reads see consistent
/// snapshots, and no `Agent` lock is involved.
#[derive(Clone)]
pub struct SessionStore {
    db: Arc<Database>,
    path: PathBuf,
}

impl SessionStore {
    /// Open (creating if needed) `<dir>/session.redb`.
    pub fn open(dir: &Path) -> Result<Self, SessionStoreError> {
        std::fs::create_dir_all(dir)?;
        let path = dir.join("session.redb");
        let db = shared_db(&path)?;
        Ok(Self { db, path })
    }

    /// Open an existing `<dir>/session.redb` for reading, creating nothing.
    ///
    /// `Ok(None)` when the directory holds no session store yet: unlike
    /// [`SessionStore::open`] this never creates the directory, the database
    /// file or any table, so a background reader (the GUI) cannot materialize a
    /// session that was never written.
    pub fn open_existing(dir: &Path) -> Result<Option<Self>, SessionStoreError> {
        let path = dir.join("session.redb");
        if !path.is_file() {
            return Ok(None);
        }
        let db = shared_db_existing(&path)?;
        Ok(Some(Self { db, path }))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn ensure_tables(db: &Database) -> Result<(), SessionStoreError> {
        let txn = db.begin_write()?;
        {
            txn.open_table(META)?;
            txn.open_table(RAW)?;
        }
        txn.commit()?;
        Ok(())
    }

    /// Write the full metadata block in one transaction.
    pub fn write_meta(&self, meta: &SessionMeta) -> Result<(), SessionStoreError> {
        let txn = self.db.begin_write()?;
        {
            let mut t = txn.open_table(META)?;
            t.insert(K_ID, meta.id.as_bytes())?;
            t.insert(K_CWD, meta.cwd.display().to_string().as_bytes())?;
            t.insert(K_CREATED_MS, meta.created_ms.to_le_bytes().as_slice())?;
            t.insert(K_MODIFIED_MS, meta.modified_ms.to_le_bytes().as_slice())?;
            t.insert(K_FIRST_PROMPT, meta.first_prompt.as_bytes())?;
        }
        txn.commit()?;
        Ok(())
    }

    /// Read the metadata block. `None` when the store was just created and
    /// never written (or is a fresh empty file).
    pub fn read_meta(&self) -> Result<Option<SessionMeta>, SessionStoreError> {
        let txn = self.db.begin_read()?;
        let t = txn.open_table(META)?;
        let Some(id) = t.get(K_ID)? else {
            return Ok(None);
        };
        let id = String::from_utf8_lossy(id.value()).to_string();
        let cwd = t
            .get(K_CWD)?
            .map(|v| PathBuf::from(String::from_utf8_lossy(v.value()).to_string()))
            .unwrap_or_default();
        let created_ms = read_u64(&t, K_CREATED_MS)?;
        let modified_ms = read_u64(&t, K_MODIFIED_MS)?;
        let first_prompt = t
            .get(K_FIRST_PROMPT)?
            .map(|v| String::from_utf8_lossy(v.value()).to_string())
            .unwrap_or_default();
        Ok(Some(SessionMeta {
            id,
            cwd,
            created_ms,
            modified_ms,
            first_prompt,
        }))
    }

    /// Update just the discovery-visible activity fields.
    pub fn touch(&self, modified_ms: u64, first_prompt: &str) -> Result<(), SessionStoreError> {
        let txn = self.db.begin_write()?;
        {
            let mut t = txn.open_table(META)?;
            t.insert(K_MODIFIED_MS, modified_ms.to_le_bytes().as_slice())?;
            t.insert(K_FIRST_PROMPT, first_prompt.as_bytes())?;
        }
        txn.commit()?;
        Ok(())
    }

    /// Append one raw audit row, returning its sequence number.
    pub fn append_raw(
        &self,
        id: &str,
        timestamp: u64,
        message: &Message,
    ) -> Result<u64, SessionStoreError> {
        self.append_raw_inner(id, timestamp, message, None)
    }

    /// Append audit and update discovery metadata with ONE durable commit.
    pub fn append_raw_with_meta(&self, id:&str, timestamp:u64, message:&Message,
        modified_ms:u64, first_prompt:&str) -> Result<u64, SessionStoreError> {
        self.append_raw_inner(id,timestamp,message,Some((modified_ms,first_prompt)))
    }

    fn append_raw_inner(&self,id:&str,timestamp:u64,message:&Message,
        touch:Option<(u64,&str)>) -> Result<u64,SessionStoreError> {
        let txn = self.db.begin_write()?;
        let seq;
        {
            let mut raw = txn.open_table(RAW)?;
            let mut meta = txn.open_table(META)?;
            seq = meta
                .get(K_RAW_COUNT)?
                .map(|v| bytes_to_u64(v.value()))
                .unwrap_or(0);
            let rec = RawRecordRef { id, timestamp, message };
            raw.insert(seq, serde_json::to_vec(&rec)?.as_slice())?;
            meta.insert(K_RAW_COUNT, (seq + 1).to_le_bytes().as_slice())?;
            if let Some((modified_ms,first_prompt)) = touch {
                meta.insert(K_MODIFIED_MS, modified_ms.to_le_bytes().as_slice())?;
                meta.insert(K_FIRST_PROMPT, first_prompt.as_bytes())?;
            }
        }
        txn.commit()?;
        Ok(seq)
    }

    /// Number of raw audit rows.
    pub fn raw_len(&self) -> Result<usize, SessionStoreError> {
        let txn = self.db.begin_read()?;
        let meta = txn.open_table(META)?;
        Ok(meta
            .get(K_RAW_COUNT)?
            .map(|v| bytes_to_u64(v.value()) as usize)
            .unwrap_or(0))
    }

    /// Full raw audit history in push order.
    pub fn raw_entries(&self) -> Result<Vec<RawEntry>, SessionStoreError> {
        let txn = self.db.begin_read()?;
        let raw = txn.open_table(RAW)?;
        let mut out = Vec::new();
        for row in raw.range::<u64>(..)? {
            let (k, v) = row?;
            let rec: RawRecord = serde_json::from_slice(v.value())?;
            out.push(RawEntry {
                seq: k.value(),
                id: rec.id,
                timestamp: rec.timestamp,
                message: rec.message,
            });
        }
        Ok(out)
    }

    /// Raw audit messages only (the API that replaces transcript string reads).
    pub fn raw_messages(&self) -> Result<Vec<Message>, SessionStoreError> {
        let txn = self.db.begin_read()?;
        let raw = txn.open_table(RAW)?;
        let mut messages = Vec::new();
        for row in raw.range::<u64>(..)? {
            let (_, value) = row?;
            let record: RawRecord = serde_json::from_slice(value.value())?;
            messages.push(record.message);
        }
        Ok(messages)
    }

    /// Bounded, paginated read of the raw audit, walking backwards.
    ///
    /// `before` is an exclusive cursor: `None` starts at the newest row, a
    /// cursor previously returned as [`RawPage::next_before`] walks older.
    /// Rows come back ascending by `seq` (oldest-first *within* the page) so a
    /// caller can append them to a transcript in order.
    ///
    /// `limit` is clamped to `1..=RAW_PAGE_MAX_ROWS`; `max_bytes` bounds the
    /// summed stored JSON size of the decoded rows. When either bound is hit
    /// the page stops early, but a cursor with any row below it always yields at
    /// least one row — `max_bytes == 0` therefore still makes progress instead
    /// of looping forever on an oversized message.
    ///
    /// `total` comes from `K_RAW_COUNT` inside the same read transaction as the
    /// rows, so a concurrent appender can only make `total` larger for later
    /// reads, never disagree with a page. Older cursors are stable: rows below
    /// `before` were all committed before `before` existed, so appending never
    /// shifts them.
    ///
    /// This is a pure read and the only raw-writing API is
    /// [`SessionStore::append_raw`] (plus the legacy import): re-reading a page
    /// can never add a row, and compaction summaries are never written here.
    pub fn raw_page_before(
        &self,
        before: Option<u64>,
        limit: usize,
        max_bytes: usize,
    ) -> Result<RawPage, SessionStoreError> {
        let limit = limit.clamp(1, RAW_PAGE_MAX_ROWS);
        let txn = self.db.begin_read()?;
        let total = {
            let meta = txn.open_table(META)?;
            meta.get(K_RAW_COUNT)?
                .map(|v| bytes_to_u64(v.value()))
                .unwrap_or(0)
        };
        let raw = txn.open_table(RAW)?;
        let upper = match before {
            Some(seq) => Bound::Excluded(seq),
            None => Bound::Unbounded,
        };
        let mut rows = raw.range::<u64>((Bound::Unbounded, upper))?;
        // Collected newest-first and reversed at the end: a redb range walks
        // forward, so its reversed walk is the bounded "N newest rows below
        // `before`" scan without materializing the whole audit.
        let mut newest_first: Vec<RawEntry> = Vec::new();
        let mut bytes = 0usize;
        let mut older_remain = false;
        for row in rows.by_ref().rev() {
            let (key, value) = row?;
            let stored = value.value().len();
            if !newest_first.is_empty() && bytes.saturating_add(stored) > max_bytes {
                // Byte budget spent. The row just pulled is older than the page
                // and is deliberately not decoded: it is only evidence that the
                // cursor must stay set.
                older_remain = true;
                break;
            }
            let rec: RawRecord = serde_json::from_slice(value.value())?;
            bytes = bytes.saturating_add(stored);
            newest_first.push(RawEntry {
                seq: key.value(),
                id: rec.id,
                timestamp: rec.timestamp,
                message: rec.message,
            });
            if newest_first.len() >= limit {
                // One look-ahead keeps `next_before == None` meaning exactly
                // "no older rows", so a pager never needs an empty last page.
                older_remain = match rows.next() {Some(row)=>{row?;true},None=>false};
                break;
            }
        }
        newest_first.reverse();
        let next_before = if older_remain {
            newest_first.first().map(|e| e.seq)
        } else {
            None
        };
        Ok(RawPage {
            before,
            entries: newest_first,
            next_before,
            total,
            bytes,
        })
    }

    /// Atomically write metadata plus a whole legacy transcript in one
    /// transaction. Used only by [`import_legacy`].
    fn bulk_import(
        &self,
        meta: &SessionMeta,
        entries: &[(String, u64, Message)],
    ) -> Result<(), SessionStoreError> {
        let txn = self.db.begin_write()?;
        {
            let mut t = txn.open_table(META)?;
            t.insert(K_ID, meta.id.as_bytes())?;
            t.insert(K_CWD, meta.cwd.display().to_string().as_bytes())?;
            t.insert(K_CREATED_MS, meta.created_ms.to_le_bytes().as_slice())?;
            t.insert(K_MODIFIED_MS, meta.modified_ms.to_le_bytes().as_slice())?;
            t.insert(K_FIRST_PROMPT, meta.first_prompt.as_bytes())?;
            t.insert(K_RAW_COUNT, (entries.len() as u64).to_le_bytes().as_slice())?;
            let mut raw = txn.open_table(RAW)?;
            for (seq, (id, timestamp, message)) in entries.iter().enumerate() {
                let rec = RawRecordRef { id, timestamp: *timestamp, message };
                raw.insert(seq as u64, serde_json::to_vec(&rec)?.as_slice())?;
            }
        }
        txn.commit()?;
        Ok(())
    }

    /// Read-back verification for a legacy import. The source JSONL is only
    /// retired when this returns true.
    fn verify_import(&self, meta: &SessionMeta, entries: &[(String, u64, Message)]) -> bool {
        match self.read_meta() {
            Ok(Some(found)) => {
                found.id == meta.id
                    && found.cwd == meta.cwd
                    && self.raw_entries().is_ok_and(|raw| {
                        raw.len() == entries.len()
                            && raw.iter().zip(entries).all(|(row, (id, time, msg))| {
                                &row.id == id && row.timestamp == *time && row.message == *msg
                            })
                    })
            }
            _ => false,
        }
    }
}

fn read_u64(
    t: &impl ReadableTable<&'static str, &'static [u8]>,
    key: &str,
) -> Result<u64, SessionStoreError> {
    Ok(t.get(key)?.map(|v| bytes_to_u64(v.value())).unwrap_or(0))
}

fn bytes_to_u64(bytes: &[u8]) -> u64 {
    let mut b = [0u8; 8];
    let n = bytes.len().min(8);
    b[..n].copy_from_slice(&bytes[..n]);
    u64::from_le_bytes(b)
}

/// Process-wide shared handle for one redb path, creating the database (and its
/// tables) if needed.
pub(crate) fn shared_db(path: &Path) -> Result<Arc<Database>, SessionStoreError> {
    shared_db_with(path, true)
}

/// Shared handle for a redb path that must already exist: never creates the
/// file and never writes the schema, so a background reader cannot materialize
/// a session directory that has no store yet.
fn shared_db_existing(path: &Path) -> Result<Arc<Database>, SessionStoreError> {
    shared_db_with(path, false)
}

fn shared_db_with(path: &Path, create: bool) -> Result<Arc<Database>, SessionStoreError> {
    let normalized = std::fs::canonicalize(path.parent().unwrap_or(Path::new(".")))?.join(
        path.file_name()
            .ok_or_else(|| std::io::Error::other("missing database filename"))?,
    );
    let path = normalized.as_path();
    if let Some(db) = cache().lock().unwrap().get(path).and_then(Weak::upgrade) {
        return Ok(db);
    }
    // Serialize concurrent first opens of the same path (redb takes a file lock
    // on create, so two racing opens of one path must not both run).
    let _guard = OPEN_LOCK.lock().unwrap();
    if let Some(db) = cache().lock().unwrap().get(path).and_then(Weak::upgrade) {
        return Ok(db);
    }
    let db = Arc::new(if create {
        crate::database::create(path)?
    } else {
        crate::database::open(path)?
    });
    if create {
        SessionStore::ensure_tables(&db)?;
    }
    cache()
        .lock()
        .unwrap()
        .insert(path.to_path_buf(), Arc::downgrade(&db));
    Ok(db)
}

/// Import a legacy `<dir>/session.jsonl` into `<dir>/session.redb`.
///
/// Validation and atomicity contract:
/// 1. the whole file is parsed before anything is written (a partial crash tail
///    on the final line is tolerated and dropped, an interior corruption is an
///    error);
/// 2. metadata + every message are written in one redb transaction;
/// 3. the store is read back and compared;
/// 4. only then is the JSONL renamed to `session.jsonl.legacy` (kept as a
///    backup, never silently deleted).
///
/// Returns `Ok(None)` when there is no JSONL to migrate.
pub fn import_legacy(dir: &Path) -> Result<Option<LegacyImport>, SessionStoreError> {
    let legacy = dir.join(LEGACY_TRANSCRIPT);
    let content = match std::fs::read_to_string(&legacy) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };

    let lines: Vec<&str> = content
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    if lines.is_empty() {
        return Err(SessionStoreError::Legacy(
            "empty transcript without a session header".into(),
        ));
    }

    let header: serde_json::Value = serde_json::from_str(lines[0])
        .map_err(|e| SessionStoreError::Legacy(format!("malformed session header: {e}")))?;
    if header.get("type").and_then(|t| t.as_str()) != Some("session") {
        return Err(SessionStoreError::Legacy(
            "first entry is not a session header".into(),
        ));
    }
    let id = header
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| SessionStoreError::Legacy("header missing id".into()))?
        .to_string();
    let cwd = header
        .get("cwd")
        .and_then(|v| v.as_str())
        .map(PathBuf::from)
        .unwrap_or_default();

    let mut entries: Vec<(String, u64, Message)> = Vec::new();
    for (i, line) in lines.iter().enumerate().skip(1) {
        let value: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            // A crash can leave a torn final line: drop the tail, never a
            // committed interior row.
            Err(_) if i == lines.len() - 1 => break,
            Err(e) => {
                return Err(SessionStoreError::Legacy(format!(
                    "line {} is not valid JSON: {e}",
                    i + 1
                )))
            }
        };
        if value.get("type").and_then(|t| t.as_str()) != Some("message") {
            continue;
        }
        let message =
            serde_json::from_value::<Message>(value.get("message").cloned().ok_or_else(|| {
                SessionStoreError::Legacy(format!("line {} has no message", i + 1))
            })?)
            .map_err(|e| {
                SessionStoreError::Legacy(format!("line {} message invalid: {e}", i + 1))
            })?;
        entries.push((
            value
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            value.get("timestamp").and_then(|v| v.as_u64()).unwrap_or(0),
            message,
        ));
    }

    let file_modified = std::fs::metadata(&legacy)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or_else(crate::message::now_millis);
    let first_prompt = first_prompt_from(entries.iter().map(|(_, _, m)| m));
    let meta = SessionMeta {
        id,
        cwd,
        created_ms: file_modified,
        modified_ms: file_modified,
        first_prompt,
    };

    let store = SessionStore::open(dir)?;
    if store.raw_len()? == 0 && store.read_meta()?.is_none() {
        store.bulk_import(&meta, &entries)?;
        if !store.verify_import(&meta, &entries) {
            return Err(SessionStoreError::Legacy(
                "read-back verification failed; JSONL preserved".into(),
            ));
        }
        let backup = dir.join(format!("{LEGACY_TRANSCRIPT}.legacy"));
        let renamed = std::fs::rename(&legacy, &backup).is_ok();
        return Ok(Some(LegacyImport {
            meta,
            entries: entries.len(),
            jsonl_retired: renamed,
        }));
    }

    // Store already holds data (e.g. import committed, then the process died
    // before the rename): only retire the JSONL when the store verifies.
    if store.verify_import(&meta, &entries) {
        let backup = dir.join(format!("{LEGACY_TRANSCRIPT}.legacy"));
        let renamed = std::fs::rename(&legacy, &backup).is_ok();
        return Ok(Some(LegacyImport {
            meta,
            entries: entries.len(),
            jsonl_retired: renamed,
        }));
    }
    Err(SessionStoreError::Legacy(
        "store already has different data; JSONL preserved".into(),
    ))
}

/// First user prompt, whitespace-normalized and truncated like the catalog's
/// discovery preview.
pub fn first_prompt_from<'a>(messages: impl Iterator<Item = &'a Message>) -> String {
    use crate::message::Role;
    for m in messages {
        if m.role == Role::User {
            return m
                .text()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .chars()
                .take(120)
                .collect();
        }
    }
    String::new()
}

#[cfg(test)]
mod tests {
    #[test]
    fn matching_metadata_and_count_cannot_retire_different_legacy_content() {
        let dir = temp("collision");
        let meta = SessionMeta {
            id: "same".into(),
            cwd: dir.clone(),
            created_ms: 1,
            modified_ms: 1,
            first_prompt: "original".into(),
        };
        let store = SessionStore::open(&dir).unwrap();
        store
            .bulk_import(&meta, &[("e0".into(), 1, Message::user_text("original"))])
            .unwrap();
        let legacy = dir.join(LEGACY_TRANSCRIPT);
        let header = serde_json::json!({"type":"session","id":"same","cwd":dir});
        let row = serde_json::json!({"type":"message","id":"e0","timestamp":1,"message":Message::user_text("different")});
        std::fs::write(&legacy, format!("{header}\n{row}\n")).unwrap();
        assert!(import_legacy(&dir).is_err());
        assert!(legacy.exists());
        assert_eq!(store.raw_messages().unwrap()[0].text(), "original");
        drop(store);
        std::fs::remove_dir_all(dir).unwrap();
    }
    use super::*;

    fn temp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "feather-sstore-{tag}-{}-{}",
            std::process::id(),
            crate::message::now_millis()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn meta(id: &str, dir: &Path) -> SessionMeta {
        SessionMeta {
            id: id.into(),
            cwd: dir.to_path_buf(),
            created_ms: 1,
            modified_ms: 2,
            first_prompt: "hello".into(),
        }
    }

    #[test]
    fn raw_paging_bounds_bytes_and_rows_and_keeps_cursors_stable_when_appending() {
        let dir=temp("raw-pages");let store=SessionStore::open(&dir).unwrap();store.write_meta(&meta("pages",&dir)).unwrap();
        for n in 0..30{store.append_raw(&format!("e{n}"),n,&Message::user_text(format!("原始消息 {n} {}","🙂".repeat(100)))).unwrap();}
        let reader=store.clone();
        let tail=reader.raw_page_before(None,7,2048).unwrap();assert!(tail.entries.len()<=7);assert!(tail.bytes<=2048||tail.entries.len()==1);
        let mut all:Vec<_>=tail.entries.iter().map(|e|e.seq).collect();let mut before=tail.next_before;
        store.append_raw("new",31,&Message::user_text("newly appended")).unwrap();
        while let Some(cursor)=before {
            let page=reader.raw_page_before(Some(cursor),5,1024).unwrap();
            assert!(!page.entries.is_empty());assert!(page.entries.iter().all(|e|e.seq<cursor));
            all.extend(page.entries.iter().map(|e|e.seq));before=page.next_before;
        }
        all.sort_unstable();assert_eq!(all,(0..30).collect::<Vec<_>>());
        let progress=reader.raw_page_before(None,0,0).unwrap();assert_eq!(progress.entries.len(),1);
        assert_eq!(progress.entries[0].seq,30);assert_eq!(progress.total,31);
        assert!(reader.raw_page_before(Some(0),usize::MAX,usize::MAX).unwrap().entries.is_empty());
        drop(reader);drop(store);std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn raw_history_round_trips_and_survives_reopen() {
        let dir = temp("raw");
        let store = SessionStore::open(&dir).unwrap();
        store.write_meta(&meta("s1", &dir)).unwrap();
        store
            .append_raw("e1", 10, &Message::user_text("one"))
            .unwrap();
        store
            .append_raw(
                "e2",
                20,
                &Message::assistant(
                    vec![crate::message::ContentBlock::Text { text: "two".into() }],
                    "m",
                    crate::message::Api::Faux,
                    crate::message::Usage::default(),
                    crate::message::StopReason::Stop,
                ),
            )
            .unwrap();
        assert_eq!(store.raw_len().unwrap(), 2);
        assert_eq!(store.raw_messages().unwrap().len(), 2);
        assert_eq!(store.raw_entries().unwrap()[0].id, "e1");
        drop(store);

        let store = SessionStore::open(&dir).unwrap();
        assert_eq!(store.read_meta().unwrap().unwrap().id, "s1");
        assert_eq!(store.raw_messages().unwrap()[1].text(), "two");
        assert!(dir.join("session.redb").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn legacy_import_is_atomic_and_retires_the_jsonl() {
        let dir = temp("legacy");
        let jsonl = dir.join(LEGACY_TRANSCRIPT);
        let mut body = String::new();
        body.push_str(&format!(
            "{{\"type\":\"session\",\"version\":1,\"id\":\"legacy-1\",\"cwd\":{}}}\n",
            serde_json::to_string(&dir.display().to_string()).unwrap()
        ));
        for i in 0..3 {
            body.push_str(&format!(
                "{{\"type\":\"message\",\"id\":\"e{i}\",\"timestamp\":{},\"message\":{}}}\n",
                i,
                serde_json::to_string(&Message::user_text(format!("line {i}"))).unwrap()
            ));
        }
        std::fs::write(&jsonl, body).unwrap();

        let imported = import_legacy(&dir).unwrap().unwrap();
        assert_eq!(imported.entries, 3);
        assert!(imported.jsonl_retired);
        assert!(!jsonl.exists(), "live JSONL removed");
        assert!(dir.join("session.jsonl.legacy").exists(), "backup kept");

        let store = SessionStore::open(&dir).unwrap();
        assert_eq!(store.read_meta().unwrap().unwrap().id, "legacy-1");
        assert_eq!(store.raw_messages().unwrap().len(), 3);
        // Second import is a no-op: nothing left to migrate.
        assert!(import_legacy(&dir).unwrap().is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn corrupt_legacy_import_leaves_jsonl_untouched() {
        let dir = temp("corrupt");
        let jsonl = dir.join(LEGACY_TRANSCRIPT);
        let good = serde_json::to_string(&Message::user_text("ok")).unwrap();
        std::fs::write(
            &jsonl,
            format!(
                "{{\"type\":\"session\",\"id\":\"x\",\"cwd\":\"/tmp\"}}\n\
                 {{\"type\":\"message\",\"id\":\"a\",\"message\":{good}}}\n\
                 this is not json\n\
                 {{\"type\":\"message\",\"id\":\"b\",\"message\":{good}}}\n"
            ),
        )
        .unwrap();
        assert!(import_legacy(&dir).is_err());
        assert!(jsonl.exists(), "unvalidated import never removes data");
        let store = SessionStore::open(&dir).unwrap();
        assert_eq!(store.raw_len().unwrap(), 0, "nothing partially written");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn torn_crash_tail_is_dropped_not_fatal() {
        let dir = temp("torn");
        let jsonl = dir.join(LEGACY_TRANSCRIPT);
        let good = serde_json::to_string(&Message::user_text("ok")).unwrap();
        std::fs::write(
            &jsonl,
            format!(
                "{{\"type\":\"session\",\"id\":\"y\",\"cwd\":\"/tmp\"}}\n\
                 {{\"type\":\"message\",\"id\":\"a\",\"message\":{good}}}\n\
                 {{\"type\":\"message\",\"id\":\"b\",\"mess\n"
            ),
        )
        .unwrap();
        let imported = import_legacy(&dir).unwrap().unwrap();
        assert_eq!(imported.entries, 1);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
