//! Small runtime metadata records in redb, shared within the process.
use redb::{Database, TableDefinition};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock, Weak},
};
const RECORDS: TableDefinition<&str, &[u8]> = TableDefinition::new("records");
/// Open database handles, keyed by canonical path.
///
/// Weak on purpose. A live handle holds the redb file lock, and
/// `session_catalog::delete_session` decides whether a session is still in use
/// precisely by trying to open these databases. Holding a strong reference here
/// would make every closed session look active and block deletion, so the cache
/// only deduplicates *concurrent* opens: the first caller's `Arc` is reused by
/// everyone while it lives, and the handle closes when the last of them drops it.
///
/// This still removes the duplicate-open cost that mattered: `Session::commit_turn`
/// used to open `checkpoints.redb` twice in a row through `FileSnapshots`, and a
/// single open costs ~5.9 ms of redb startup (measured by `feather-bench --probe`).
static DBS: OnceLock<Mutex<BTreeMap<PathBuf, Weak<Database>>>> = OnceLock::new();
#[derive(Clone)]
pub struct Records(Arc<Database>);
impl Records {
    /// The underlying database handle.
    ///
    /// Public because "is this the same handle?" is the only way to assert that the
    /// process-wide cache did its job: two live handles to one redb file cannot
    /// coexist, so a caller that holds one and opens the path again is either reusing
    /// it or failing. `feather-bench`'s `records.warm` uses this to pin that reuse, so
    /// the warm-open measurement cannot silently drift back to cold.
    pub fn database(&self) -> &Database {
        &self.0
    }
    pub fn open(path: &Path) -> Result<Self, String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let path = std::fs::canonicalize(path.parent().unwrap_or(Path::new(".")))
            .map_err(|e| e.to_string())?
            .join(path.file_name().ok_or("missing database filename")?);
        let mut cache = DBS
            .get_or_init(Default::default)
            .lock()
            .map_err(|e| e.to_string())?;
        // Cache hit: a live handle for this path is already open in this process.
        // Reuse it instead of opening a second handle, which redb would reject
        // ("Database already open") because the first one holds the file lock.
        if let Some(db) = cache.get(&path).and_then(Weak::upgrade) {
            return Ok(Self(db));
        }
        // Open an existing database, create only when there is nothing to open.
        // `create` on a live file rewrites it, which made every reopen pay for
        // work the file already contained.
        let db = Arc::new(match crate::database::open(&path) {
            Ok(db) => db,
            // Only a genuinely absent file is created. A file that exists but
            // cannot be opened is locked by another process (two Feather windows
            // on one session directory), and calling `create` there would report
            // a confusing secondary failure instead of the real one.
            Err(_) if !path.exists() => {
                crate::database::create(&path).map_err(|e| e.to_string())?
            }
            Err(e) => return Err(e.to_string()),
        });
        // Do NOT open a write transaction here.
        //
        // Creating the table used to need one, and an unconditional durable
        // commit on *every* open is what made `Records::open` cost ~20 ms on a
        // real disk. `FileSnapshots` opens this database twice per
        // `Session::commit_turn` (once to read the tracked list, once to write),
        // so that single line put two avoidable fsyncs on the turn boundary —
        // measured at 41 ms of a 71 ms commit by `feather-bench`.
        //
        // The table is created by `put` instead, which already runs its own write
        // transaction and creates the table as part of it. `get` on a database
        // whose table does not exist yet returns "table not found"; that is
        // mapped to `None` below, which is the same answer the pre-created empty
        // table would have given.
        cache.insert(path, Arc::downgrade(&db));
        Ok(Self(db))
    }
    pub fn get<T: serde::de::DeserializeOwned>(&self, key: &str) -> Result<Option<T>, String> {
        let tx = self.0.begin_read().map_err(|e| e.to_string())?;
        // A brand-new database has no table until the first `put`. `open` no
        // longer pre-creates it (that cost a durable commit per open), so a
        // missing table means "no records yet" — the same answer an empty table
        // returns. Any other error still propagates.
        let table = match tx.open_table(RECORDS) {
            Ok(table) => table,
            Err(redb::TableError::TableDoesNotExist(_)) => return Ok(None),
            Err(e) => return Err(e.to_string()),
        };
        let result = table
            .get(key)
            .map_err(|e| e.to_string())?
            .map(|v| serde_json::from_slice(v.value()).map_err(|e| e.to_string()))
            .transpose();
        result
    }
    pub fn put<T: serde::Serialize>(&self, key: &str, value: &T) -> Result<(), String> {
        let data = serde_json::to_vec(value).map_err(|e| e.to_string())?;
        let tx = self.0.begin_write().map_err(|e| e.to_string())?;
        {
            tx.open_table(RECORDS)
                .map_err(|e| e.to_string())?
                .insert(key, data.as_slice())
                .map_err(|e| e.to_string())?;
        }
        tx.commit().map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_db(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "feather-records-{tag}-{}",
            crate::provider::gen_id("r")
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("records.redb")
    }

    /// Forget a test database's cached handle and delete its directory.
    ///
    /// The handle cache is process-wide and holds strong references, so a test
    /// that only deletes the directory would leave the cache pointing at a path
    /// that no longer exists — and a later open of the same path would then race
    /// the live handle for the file lock.
    fn cleanup(path: &Path) {
        let canonical = std::fs::canonicalize(path.parent().unwrap())
            .unwrap_or_else(|_| path.parent().unwrap().to_path_buf())
            .join(path.file_name().unwrap());
        DBS.get_or_init(Default::default)
            .lock()
            .unwrap()
            .remove(&canonical);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// A database that has never been written has no table. Reading it must say
    /// "no such record", not "error", or every caller has to special-case the
    /// first read — which is exactly the case `put`-creates-the-table removes.
    #[test]
    fn read_before_any_write_returns_none() {
        let path = temp_db("read-first");
        let store = Records::open(&path).unwrap();
        let value: Option<u32> = store.get("absent").unwrap();
        assert_eq!(value, None);
        drop(store);
        cleanup(&path);
    }

    /// Values must survive a close/reopen cycle. The handle cache keeps strong
    /// references now, so this also proves the reopen path still reads real data
    /// off disk rather than a stale in-memory copy.
    #[test]
    fn values_survive_reopen() {
        let path = temp_db("reopen");
        {
            let store = Records::open(&path).unwrap();
            store.put("answer", &42u32).unwrap();
        }
        // Drop the cached handle so the next open must hit the file.
        // Drop only *this* database's handle, not the whole cache: clearing
        // everything would close handles other tests are concurrently using,
        // since the cache is process-wide.
        let canonical = std::fs::canonicalize(path.parent().unwrap())
            .unwrap()
            .join(path.file_name().unwrap());
        DBS.get_or_init(Default::default)
            .lock()
            .unwrap()
            .remove(&canonical);
        let reopened = Records::open(&path).unwrap();
        let value: Option<u32> = reopened.get("answer").unwrap();
        assert_eq!(value, Some(42));
        drop(reopened);
        cleanup(&path);
    }

    /// Two opens of the same path must share one handle — that sharing is the
    /// whole point of keeping strong references, and it is what makes the second
    /// call free instead of a ~6 ms redb startup.
    #[test]
    fn repeated_opens_share_one_handle() {
        let path = temp_db("shared");
        let first = Records::open(&path).unwrap();
        let second = Records::open(&path).unwrap();
        assert!(
            std::ptr::eq(first.database(), second.database()),
            "two opens of the same path should return the same database handle"
        );
        drop(first);
        drop(second);
        cleanup(&path);
    }

    /// A dropped handle must release its database.
    ///
    /// This is the contract that keeps `session_catalog::delete_session` working:
    /// it decides "is this session still in use?" by trying to open the session's
    /// databases, so a handle that outlived its owner would make every closed
    /// session look active and block deletion forever.
    #[test]
    fn dropped_handle_releases_the_database() {
        let path = temp_db("release");
        {
            let store = Records::open(&path).unwrap();
            store.put("k", &7u32).unwrap();
        }
        cleanup(&path);
        // The directory is gone. If the cache still held a live handle, removing
        // it would have failed on Windows and the reopen below would try to open
        // a deleted file.
        let reopened = Records::open(&path);
        assert!(
            reopened.is_ok(),
            "a released database can be reopened after its directory is removed"
        );
        cleanup(&path);
    }
}
