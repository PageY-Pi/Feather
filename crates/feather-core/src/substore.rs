//! Subagent working data on redb — the same storage family as sessions and
//! snapshots. One `subagents.redb` per session directory holds every
//! subagent run: mission (label + prompt), the full transcript as line
//! records, and the final output.
//!
//! Replaces the loose `.pi-subagents/` file convention: subagent data is
//! session machinery, so it lives beside the other session machinery in the
//! feather home instead of scattering files into the project tree.

use redb::{Database, ReadableTable, TableDefinition};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const SUBS: TableDefinition<'static, &str, &[u8]> = TableDefinition::new("subs");
const SUB_LINES: TableDefinition<'static, (u64, u64), &[u8]> = TableDefinition::new("sub_lines");

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubRecord {
    pub id: String,
    pub label: String,
    pub prompt: String,
    pub model: String,
    pub started_ms: u64,
    pub finished_ms: Option<u64>,
    pub output: Option<String>,
    pub error: Option<String>,
    pub lines: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum SubStoreError {
    #[error("redb error: {0}")]
    Db(String),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("no subagent record for {0}")]
    NotFound(String),
}

impl From<redb::DatabaseError> for SubStoreError {
    fn from(e: redb::DatabaseError) -> Self {
        Self::Db(e.to_string())
    }
}

impl From<redb::TransactionError> for SubStoreError {
    fn from(e: redb::TransactionError) -> Self {
        Self::Db(e.to_string())
    }
}

impl From<redb::TableError> for SubStoreError {
    fn from(e: redb::TableError) -> Self {
        Self::Db(e.to_string())
    }
}

impl From<redb::StorageError> for SubStoreError {
    fn from(e: redb::StorageError) -> Self {
        Self::Db(e.to_string())
    }
}

impl From<redb::CommitError> for SubStoreError {
    fn from(e: redb::CommitError) -> Self {
        Self::Db(e.to_string())
    }
}

pub struct SubagentStore {
    db: std::sync::Arc<Database>,
    #[allow(dead_code)]
    path: PathBuf,
}

impl SubagentStore {
    /// Open (creating if needed) `<session_dir>/subagents.redb`.
    pub fn open(session_dir: &Path) -> Result<Self, SubStoreError> {
        std::fs::create_dir_all(session_dir).map_err(|e| SubStoreError::Db(e.to_string()))?;
        let path = session_dir.join("subagents.redb");
        Ok(Self {
            db: crate::session_store::shared_db(&path)
                .map_err(|e| SubStoreError::Db(e.to_string()))?,
            path,
        })
    }

    fn rec(&self, id: &str) -> Result<Option<SubRecord>, SubStoreError> {
        let txn = self.db.begin_read()?;
        let subs = txn.open_table(SUBS)?;
        // A corrupt record must surface, not masquerade as "missing": the
        // terminal `finish`/`fail` write must be able to detect the failure.
        match subs.get(id)? {
            Some(b) => Ok(Some(serde_json::from_slice(b.value())?)),
            None => Ok(None),
        }
    }

    fn put_rec(&self, rec: &SubRecord) -> Result<(), SubStoreError> {
        let txn = self.db.begin_write()?;
        {
            let mut subs = txn.open_table(SUBS)?;
            let bytes = serde_json::to_vec(rec)?;
            subs.insert(rec.id.as_str(), bytes.as_slice())?;
        }
        txn.commit()?;
        Ok(())
    }

    /// Record the start of a run (mission).
    pub fn start(
        &self,
        id: &str,
        label: &str,
        prompt: &str,
        model: &str,
    ) -> Result<(), SubStoreError> {
        self.put_rec(&SubRecord {
            id: id.to_string(),
            label: label.to_string(),
            prompt: prompt.to_string(),
            model: model.to_string(),
            started_ms: crate::message::now_millis(),
            finished_ms: None,
            output: None,
            error: None,
            lines: 0,
        })
    }

    /// Append one transcript line (a serialized message entry).
    pub fn push_line(&self, id: &str, no: u64, line: &str) -> Result<(), SubStoreError> {
        let txn = self.db.begin_write()?;
        {
            let mut subs = txn.open_table(SUBS)?;
            let mut rec: SubRecord = {
                let bytes = subs
                .get(id)?
                .ok_or_else(|| SubStoreError::NotFound(id.into()))?
                ;
                serde_json::from_slice(bytes.value())?
            };
            rec.lines = rec.lines.max(no + 1);
            let bytes = serde_json::to_vec(&rec)?;
            subs.insert(id, bytes.as_slice())?;
            txn.open_table(SUB_LINES)?
                .insert((id_key(id), no), line.as_bytes())?;
        }
        txn.commit()?;
        Ok(())
    }

    /// Persist one child message as structured JSON in run order.
    pub fn push_message(
        &self,
        id: &str,
        no: u64,
        msg: &crate::message::Message,
    ) -> Result<(), SubStoreError> {
        let line = serde_json::to_string(msg)?;
        self.push_line(id, no, &line)
    }

    pub fn messages(&self, id: &str) -> Result<Vec<crate::message::Message>, SubStoreError> {
        let mut out = Vec::new();
        let key=id_key(id);
        let txn=self.db.begin_read()?;
        let lines=txn.open_table(SUB_LINES)?;
        for row in lines.range((key,0)..=(key,u64::MAX))? {
            let (_,value)=row?;
            if let Ok(msg) = serde_json::from_slice::<crate::message::Message>(value.value()) {
                out.push(msg);
            }
        }
        Ok(out)
    }
    /// Record a successful finish with the subagent's final text.
    pub fn finish(&self, id: &str, output: &str) -> Result<(), SubStoreError> {
        let Some(mut rec) = self.rec(id)? else {
            return Err(SubStoreError::NotFound(id.to_string()));
        };
        rec.finished_ms = Some(crate::message::now_millis());
        rec.output = Some(output.to_string());
        self.put_rec(&rec)
    }

    /// Record a failed run.
    pub fn fail(&self, id: &str, error: &str) -> Result<(), SubStoreError> {
        let Some(mut rec) = self.rec(id)? else {
            return Err(SubStoreError::NotFound(id.to_string()));
        };
        rec.finished_ms = Some(crate::message::now_millis());
        rec.error = Some(error.to_string());
        self.put_rec(&rec)
    }

    pub fn get(&self, id: &str) -> Result<Option<SubRecord>, SubStoreError> {
        self.rec(id)
    }

    /// All runs, oldest first.
    pub fn list(&self) -> Result<Vec<SubRecord>, SubStoreError> {
        let txn = self.db.begin_read()?;
        let subs = txn.open_table(SUBS)?;
        let mut out = Vec::new();
        for row in subs.iter()? {
            let (_, v) = row?;
            if let Ok(r) = serde_json::from_slice::<SubRecord>(v.value()) {
                out.push(r);
            }
        }
        Ok(out)
    }

    /// The full transcript of one run (line records in order).
    pub fn transcript(&self, id: &str) -> Result<Vec<String>, SubStoreError> {
        let key = id_key(id);
        let txn = self.db.begin_read()?;
        let lines = txn.open_table(SUB_LINES)?;
        let mut out = Vec::new();
        for row in lines.range((key, 0)..=(key, u64::MAX))? {
            let (_, v) = row?;
            out.push(String::from_utf8_lossy(v.value()).to_string());
        }
        Ok(out)
    }
}

/// Pack an alphanumeric subagent id into a u64 key (FNV-1a) — stable within
/// a session's store.
fn id_key(id: &str) -> u64 {
    crate::message::fnv1a(id.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "feather-substore-{tag}-{}-{}",
            std::process::id(),
            crate::message::now_millis()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn subagent_runs_persist_full_lifecycle() {
        let dir = temp("s");
        let store = SubagentStore::open(&dir).unwrap();

        store
            .start("sub-1", "refactor", "do the thing", "m")
            .unwrap();
        store.push_line("sub-1", 0, r#"{"role":"user"}"#).unwrap();
        store
            .push_line("sub-1", 1, r#"{"role":"assistant"}"#)
            .unwrap();
        store.finish("sub-1", "done: all green").unwrap();

        let rec = store.get("sub-1").unwrap().unwrap();
        assert_eq!(rec.label, "refactor");
        assert_eq!(rec.lines, 2);
        assert_eq!(rec.output.as_deref(), Some("done: all green"));
        assert!(rec.error.is_none() && rec.finished_ms.is_some());

        // transcript round-trips
        let lines = store.transcript("sub-1").unwrap();
        assert_eq!(lines.len(), 2);
        assert!(lines[1].contains("assistant"));

        // list is ordered and includes the run
        let all = store.list().unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].id, "sub-1");

        // failure path
        store.start("sub-2", "boom", "x", "m").unwrap();
        store.fail("sub-2", "timeout").unwrap();
        let rec = store.get("sub-2").unwrap().unwrap();
        assert_eq!(rec.error.as_deref(), Some("timeout"));
        assert!(rec.output.is_none());

        // reopen: everything survives on disk
        drop(store);
        let store = SubagentStore::open(&dir).unwrap();
        assert_eq!(store.list().unwrap().len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn terminal_record_write_fails_loudly_when_record_is_missing() {
        let dir = temp("missing");
        let store = SubagentStore::open(&dir).unwrap();
        // Writing the terminal state for a run that was never started must not
        // silently succeed — that would drop a durable error record.
        assert!(store.finish("never-started", "out").is_err());
        assert!(store.fail("never-started", "boom").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
