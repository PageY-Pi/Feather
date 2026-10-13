//! Small runtime metadata records in redb, shared within the process.
use redb::{Database, TableDefinition};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock, Weak},
};
const RECORDS: TableDefinition<&str, &[u8]> = TableDefinition::new("records");
static DBS: OnceLock<Mutex<BTreeMap<PathBuf, Weak<Database>>>> = OnceLock::new();
#[derive(Clone)]
pub struct Records(Arc<Database>);
impl Records {
    pub(crate) fn database(&self) -> &Database {
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
        if let Some(db) = cache.get(&path).and_then(Weak::upgrade) {
            return Ok(Self(db));
        }
        let db = Arc::new(crate::database::create(&path).map_err(|e| e.to_string())?);
        let tx = db.begin_write().map_err(|e| e.to_string())?;
        {
            tx.open_table(RECORDS).map_err(|e| e.to_string())?;
        }
        tx.commit().map_err(|e| e.to_string())?;
        cache.insert(path, Arc::downgrade(&db));
        Ok(Self(db))
    }
    pub fn get<T: serde::de::DeserializeOwned>(&self, key: &str) -> Result<Option<T>, String> {
        let tx = self.0.begin_read().map_err(|e| e.to_string())?;
        let table = tx.open_table(RECORDS).map_err(|e| e.to_string())?;
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
