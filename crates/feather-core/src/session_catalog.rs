//! Session discovery through a global redb catalog.
//!
//! Discovery must never open the active session's `session.redb` (it is locked
//! by the running process). Instead every session mirrors its discovery
//! metadata into `<feather_home>/sessions/catalog.redb`: identity, working
//! directory, created/modified timestamps and the first prompt. The catalog is
//! only *briefly* open — writes and reads take the process-shared handle from a
//! `Weak` cache, so two callers in one process reuse the same `Database` while
//! an idle catalog stays closed and reachable from another process.
//!
//! [`SessionInfo`] is the GUI/agent-facing API and keeps its original fields
//! (`id`, `dir`, `modified_ms`, `first_prompt`); `created_ms` and `cwd` are
//! additive.

use crate::message::Message;
use crate::session::SessionError;
use crate::session_store::{self, SessionMeta};
use redb::{Database, ReadableTable, TableDefinition};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, Weak};

const CATALOG: TableDefinition<'static, &str, &[u8]> = TableDefinition::new("sessions");

#[derive(Debug, Clone)]
pub struct SessionInfo {
    pub id: String,
    pub dir: PathBuf,
    pub modified_ms: u128,
    pub first_prompt: String,
    /// Additive metadata (GUI can ignore it).
    pub created_ms: u128,
    pub cwd: PathBuf,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct CatalogEntry {
    id: String,
    dir: PathBuf,
    cwd: PathBuf,
    created_ms: u64,
    modified_ms: u64,
    first_prompt: String,
}

impl CatalogEntry {
    fn info(&self) -> SessionInfo {
        SessionInfo {
            id: self.id.clone(),
            dir: self.dir.clone(),
            modified_ms: self.modified_ms as u128,
            first_prompt: self.first_prompt.clone(),
            created_ms: self.created_ms as u128,
            cwd: self.cwd.clone(),
        }
    }
}

static CACHE: OnceLock<Mutex<HashMap<PathBuf, Weak<Database>>>> = OnceLock::new();
static OPEN_LOCK: Mutex<()> = Mutex::new(());

fn cache() -> &'static Mutex<HashMap<PathBuf, Weak<Database>>> {
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn catalog_path(home: &Path) -> PathBuf {
    home.join("sessions").join("catalog.redb")
}

/// Shared handle for the catalog database, opening it if the cached handle has
/// been dropped. Retries a few times: another process may be mid-commit.
fn open_catalog(path: &Path) -> Result<Arc<Database>, SessionError> {
    if let Some(db) = cache().lock().unwrap().get(path).and_then(Weak::upgrade) {
        return Ok(db);
    }
    let _guard = OPEN_LOCK.lock().unwrap();
    if let Some(db) = cache().lock().unwrap().get(path).and_then(Weak::upgrade) {
        return Ok(db);
    }
    let mut last = String::new();
    for _ in 0..40 {
        match crate::database::create(path) {
            Ok(db) => {
                let db = Arc::new(db);
                ensure_table(&db)?;
                cache()
                    .lock()
                    .unwrap()
                    .insert(path.to_path_buf(), Arc::downgrade(&db));
                return Ok(db);
            }
            Err(e) => {
                last = e.to_string();
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        }
    }
    Err(SessionError::Catalog(last))
}

fn ensure_table(db: &Database) -> Result<(), SessionError> {
    let txn = db
        .begin_write()
        .map_err(|e| SessionError::Catalog(e.to_string()))?;
    {
        txn.open_table(CATALOG)
            .map_err(|e| SessionError::Catalog(e.to_string()))?;
    }
    txn.commit()
        .map_err(|e| SessionError::Catalog(e.to_string()))?;
    Ok(())
}

/// Mirror one session's discovery metadata into the global catalog.
pub fn register_session(
    home: &Path,
    dir: &Path,
    meta: &SessionMeta,
) -> Result<(), SessionError> {
    let path = catalog_path(home);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let db = open_catalog(&path)?;
    let entry = CatalogEntry {
        id: meta.id.clone(),
        dir: dir.to_path_buf(),
        cwd: meta.cwd.clone(),
        created_ms: meta.created_ms,
        modified_ms: meta.modified_ms,
        first_prompt: meta.first_prompt.clone(),
    };
    let txn = db
        .begin_write()
        .map_err(|e| SessionError::Catalog(e.to_string()))?;
    {
        let mut table = txn
            .open_table(CATALOG)
            .map_err(|e| SessionError::Catalog(e.to_string()))?;
        let bytes = serde_json::to_vec(&entry)?;
        table
            .insert(entry.id.as_str(), bytes.as_slice())
            .map_err(|e| SessionError::Catalog(e.to_string()))?;
    }
    txn.commit()
        .map_err(|e| SessionError::Catalog(e.to_string()))?;
    Ok(())
}

/// Delete only an inactive direct child session, never an arbitrary path.
pub fn delete_session(home: &Path, dir: &Path) -> Result<(), SessionError> {
    let root = std::fs::canonicalize(home.join("sessions"))?;
    let target = std::fs::canonicalize(dir)?;
    let meta = std::fs::symlink_metadata(dir)?;
    if meta.file_type().is_symlink() || target.parent() != Some(root.as_path()) || !meta.is_dir() {
        return Err(SessionError::Catalog("Refusing to delete a path outside session history".into()));
    }
    let id = target.file_name().and_then(|s|s.to_str()).ok_or_else(||SessionError::Catalog("Invalid session ID".into()))?.to_string();
    if !id.starts_with('s') || (!target.join("session.redb").is_file() && !target.join("session.jsonl").is_file()) {
        return Err(SessionError::Catalog("Not a session directory".into()));
    }
    let mut handles = Vec::new();
    for name in ["session.redb","tasks.redb","checkpoints.redb","subagents.redb"] {
        let path=target.join(name);
        if path.exists() { handles.push(crate::database::open(path).map_err(|_|SessionError::Catalog("Session is active or locked; close it before deleting".into()))?); }
    }
    drop(handles);
    let trash=root.join(format!(".deleted-{id}-{}",crate::message::now_millis()));
    std::fs::rename(&target,&trash)?;
    let result = (|| {
        let db=open_catalog(&catalog_path(home))?;
        let tx=db.begin_write().map_err(|e|SessionError::Catalog(e.to_string()))?;
        {
            let mut table=tx.open_table(CATALOG).map_err(|e|SessionError::Catalog(e.to_string()))?;
            table.remove(id.as_str()).map_err(|e|SessionError::Catalog(e.to_string()))?;
        }
        tx.commit().map_err(|e|SessionError::Catalog(e.to_string()))?;
        Ok::<_,SessionError>(())
    })();
    if let Err(e)=result {let _=std::fs::rename(&trash,&target);return Err(e);}
    std::fs::remove_dir_all(trash)?;
    Ok(())
}
/// Every catalog entry, keyed by session id.
fn catalog_entries(home: &Path) -> Result<HashMap<String, CatalogEntry>, SessionError> {
    let path = catalog_path(home);
    if !path.exists() {
        return Ok(HashMap::new());
    }
    let db = open_catalog(&path)?;
    let txn = db
        .begin_read()
        .map_err(|e| SessionError::Catalog(e.to_string()))?;
    let table = txn
        .open_table(CATALOG)
        .map_err(|e| SessionError::Catalog(e.to_string()))?;
    let mut out = HashMap::new();
    for row in table
        .iter()
        .map_err(|e| SessionError::Catalog(e.to_string()))?
    {
        let (_, v) = row.map_err(|e| SessionError::Catalog(e.to_string()))?;
        if let Ok(entry) = serde_json::from_slice::<CatalogEntry>(v.value()) {
            out.insert(entry.id.clone(), entry);
        }
    }
    Ok(out)
}

/// Sessions for `cwd`, newest activity first. Reads the catalog and, when a
/// session predates the catalog, migrates its legacy JSONL header/rediscovery
/// metadata on the fly (best effort; never opens a locked active database).
pub fn list_sessions(home: &Path, cwd: &Path) -> Result<Vec<SessionInfo>, SessionError> {
    let root = home.join("sessions");
    let cwd = std::fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
    let mut entries = catalog_entries(home)?;

    // Reconcile with on-disk directories: register legacy sessions that were
    // never opened by this build, and drop catalog rows whose dir is gone.
    if let Ok(dir_entries) = std::fs::read_dir(&root) {
        for dir in dir_entries.flatten().map(|e| e.path()) {
            if dir.file_name().and_then(|s|s.to_str()).map(|s|s.starts_with(".deleted-")).unwrap_or(false) {continue;}
            if !dir.is_dir() {
                continue;
            }
            if entries.values().any(|e| e.dir == dir) {
                continue;
            }
            if let Some(entry) = discover_dir(&dir) {
                let _ = register_session(home, &dir, &SessionMeta {
                    id: entry.id.clone(),
                    cwd: entry.cwd.clone(),
                    created_ms: entry.created_ms,
                    modified_ms: entry.modified_ms,
                    first_prompt: entry.first_prompt.clone(),
                });
                entries.insert(entry.id.clone(), entry);
            }
        }
    }

    let mut sessions: Vec<SessionInfo> = entries
        .values()
        .filter(|e| e.dir.is_dir())
        .map(CatalogEntry::info)
        .filter(|info| {
            let folder =
                std::fs::canonicalize(&info.cwd).unwrap_or_else(|_| info.cwd.clone());
            folder == cwd
        })
        .collect();
    sessions.sort_by(|a, b| {
        b.modified_ms
            .cmp(&a.modified_ms)
            .then(a.id.cmp(&b.id))
    });
    Ok(sessions)
}

/// Best-effort metadata for a session directory the catalog does not know yet:
/// either its `session.redb` (if unlocked) or a legacy `session.jsonl` header.
fn discover_dir(dir: &Path) -> Option<CatalogEntry> {
    if dir.join("session.redb").exists() {
        if let Ok(store) = session_store::SessionStore::open(dir) {
            if let Ok(Some(meta)) = store.read_meta() {
                return Some(CatalogEntry {
                    id: meta.id,
                    dir: dir.to_path_buf(),
                    cwd: meta.cwd,
                    created_ms: meta.created_ms,
                    modified_ms: meta.modified_ms,
                    first_prompt: meta.first_prompt,
                });
            }
        }
    }
    let jsonl = dir.join("session.jsonl");
    let file = std::fs::File::open(&jsonl).ok()?;
    let modified_ms = file
        .metadata()
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let mut messages: Vec<Message> = Vec::new();
    let mut id = String::new();
    let mut cwd = PathBuf::new();
    use std::io::BufRead;
    for line in std::io::BufReader::new(file).lines().take(4096).map_while(Result::ok) {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        match value.get("type").and_then(|t| t.as_str()) {
            Some("session") => {
                id = value
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                cwd = value
                    .get("cwd")
                    .and_then(|v| v.as_str())
                    .map(PathBuf::from)
                    .unwrap_or_default();
            }
            Some("message") => {
                if let Ok(m) = serde_json::from_value::<Message>(value["message"].clone()) {
                    messages.push(m);
                }
            }
            _ => {}
        }
    }
    if id.is_empty() {
        return None;
    }
    Some(CatalogEntry {
        id,
        dir: dir.to_path_buf(),
        cwd,
        created_ms: modified_ms,
        modified_ms,
        first_prompt: session_store::first_prompt_from(messages.iter()),
    })
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::Session;

    #[test]
    fn discovery_does_not_lock_active_session_and_filters_cwd() {
        let root = std::env::temp_dir().join(crate::provider::gen_id("catalog"));
        std::fs::create_dir_all(&root).unwrap();
        let mut active = Session::new(&root, &root).unwrap();
        active.push(Message::user_text("hello\nworld")).unwrap();
        let other = Session::new(&root, &root.join("other")).unwrap();
        let infos = list_sessions(&root, &root).unwrap();
        assert_eq!(infos.len(), 1);
        assert_eq!(infos[0].id, active.id());
        assert_eq!(infos[0].first_prompt, "hello world");
        // metadata extras are populated from the redb catalog
        assert_eq!(infos[0].cwd, root);
        assert!(infos[0].created_ms > 0);
        drop((active, other));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn deletion_rejects_active_and_external_paths_and_removes_history() {
        let home=std::env::temp_dir().join(crate::provider::gen_id("delete-session"));
        std::fs::create_dir_all(&home).unwrap();
        let mut s=Session::new(&home,&home).unwrap();
        s.push(Message::user_text("delete me")).unwrap();s.commit_turn().unwrap();
        let dir=s.dir().to_path_buf();
        assert!(delete_session(&home,&dir).is_err());
        assert!(dir.exists());
        assert!(delete_session(&home,&home).is_err());
        drop(s);
        delete_session(&home,&dir).unwrap();
        assert!(!dir.exists());
        assert!(list_sessions(&home,&home).unwrap().is_empty());
        std::fs::remove_dir_all(home).unwrap();
    }
    #[test]
    fn catalog_covers_sessions_created_before_discovery() {
        let root = std::env::temp_dir().join(crate::provider::gen_id("catalog2"));
        std::fs::create_dir_all(&root).unwrap();
        let mut s = Session::new(&root, &root).unwrap();
        s.push(Message::user_text("persisted prompt")).unwrap();
        s.commit_turn().unwrap();
        // Drop the database handles: a fresh process would see the same catalog.
        let id = s.id().to_string();
        let dir = s.dir().to_path_buf();
        drop(s);
        let infos = list_sessions(&root, &root).unwrap();
        assert_eq!(infos.len(), 1);
        assert_eq!(infos[0].id, id);
        assert_eq!(infos[0].dir, dir);
        assert_eq!(infos[0].first_prompt, "persisted prompt");
        std::fs::remove_dir_all(root).unwrap();
    }
}
