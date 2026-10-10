//! Transactional global configuration. Legacy JSON is imported once, never read again.
//! A separate OS lock serializes short-lived redb opens across CLI/GUI processes.
use redb::{Database, ReadableTable, TableDefinition};
use serde_json::{json, Value};
use std::{fs, io, path::Path};
const DOCUMENTS: TableDefinition<&str, &str> = TableDefinition::new("documents");
/// Only effort capability presets are removed; routing, secrets, limits,
/// format compatibility and the user's selected effort settings are preserved.
pub fn strip_effort_presets(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for key in ["reasoning", "thinkingLevels", "thinking_levels", "thinkingLevelMap", "thinking_level_map", "supportsReasoningEffort"] { map.remove(key); }
            for v in map.values_mut() { strip_effort_presets(v); }
        }
        Value::Array(list) => for v in list { strip_effort_presets(v); },
        _ => {}
    }
}
fn with_db<T>(home: &Path, f: impl FnOnce(&Database) -> io::Result<T>) -> io::Result<T> {
    fs::create_dir_all(home)?;
    #[cfg(unix)] {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(home, fs::Permissions::from_mode(0o700))?;
    }
    let lock = fs::OpenOptions::new().create(true).truncate(false).read(true).write(true)
        .open(home.join("config.lock"))?;
    fs2::FileExt::lock_exclusive(&lock)?;
    let db = feather_core::database::create(home.join("config.redb")).map_err(io::Error::other)?;
    let mut cleanup:Vec<(std::path::PathBuf,String)>=Vec::new();
    // redb itself holds an exclusive file lock; shared config.lock would let
    // two processes open conflicting database handles. Keep the short OS lock,
    // but avoid a write transaction/commit on already-migrated pure reads.
    let migrations_complete={
        let tx=db.begin_read().map_err(io::Error::other)?;
        match tx.open_table(DOCUMENTS) {
            Ok(table)=>{
                let mut complete=true;
                for key in ["migration_v1","manual_effort_v3","auth_migration_v4",
                    "redb-only-keybindings-v1","redb-only-subagent-policy-v1","redb-only-compaction-model-v1"] {
                    if table.get(key).map_err(io::Error::other)?.is_none(){complete=false;break;}
                }
                complete
            },
            Err(redb::TableError::TableDoesNotExist(_))=>false,
            Err(error)=>return Err(io::Error::other(error)),
        }
    };
    let legacy_present=["settings","models","auth","keybindings","subagent-policy","compaction-model"].iter().any(|key|
        ["json","json.migrated"].iter().any(|suffix|home.join(format!("{key}.{suffix}")).exists()));
    if migrations_complete&&!legacy_present {return f(&db);}
    let tx = db.begin_write().map_err(io::Error::other)?;
    {
        let mut table = tx.open_table(DOCUMENTS).map_err(io::Error::other)?;
        if table.get("migration_v1").map_err(io::Error::other)?.is_none() {
            for key in ["settings", "models"] {
                let path = home.join(format!("{key}.json"));
                let value = match fs::read_to_string(path) {
                    Ok(text) => {
                        let value: Value = serde_json::from_str(text.trim_start_matches('\u{feff}')).map_err(io::Error::other)?;
                        if !value.is_object() { return Err(io::Error::other("legacy config must be an object")); }
                        value
                    }
                    Err(e) if e.kind() == io::ErrorKind::NotFound => json!({}),
                    Err(e) => return Err(e),
                };
                let serialized = serde_json::to_string(&value).map_err(io::Error::other)?;
                table.insert(key, serialized.as_str()).map_err(io::Error::other)?;
            }
            table.insert("migration_v1", "true").map_err(io::Error::other)?;
        }
        if table.get("manual_effort_v3").map_err(io::Error::other)?.is_none() {
            let mut models: Value = table.get("models").map_err(io::Error::other)?
                .map(|v| serde_json::from_str(v.value()).map_err(io::Error::other)).transpose()?.unwrap_or_else(|| json!({}));
            strip_effort_presets(&mut models);
            let text=serde_json::to_string(&models).map_err(io::Error::other)?;
            table.insert("models",text.as_str()).map_err(io::Error::other)?;
            table.remove("capabilities").map_err(io::Error::other)?;
            table.insert("manual_effort_v3","true").map_err(io::Error::other)?;
        }
        if table.get("auth_migration_v4").map_err(io::Error::other)?.is_none() {
            let value = match fs::read_to_string(home.join("auth.json")) {
                Ok(text) => {
                    let value: Value = serde_json::from_str(text.trim_start_matches('\u{feff}')).map_err(io::Error::other)?;
                    if !value.is_object() { return Err(io::Error::other("legacy auth must be an object")); }
                    value
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => json!({}),
                Err(e) => return Err(e),
            };
            let serialized = serde_json::to_string(&value).map_err(io::Error::other)?;
            table.insert("auth", serialized.as_str()).map_err(io::Error::other)?;
            table.insert("auth_migration_v4", "true").map_err(io::Error::other)?;
        }
        for key in ["keybindings","subagent-policy","compaction-model"] {
            let marker=format!("redb-only-{key}-v1");
            if table.get(marker.as_str()).map_err(io::Error::other)?.is_none(){
                if table.get(key).map_err(io::Error::other)?.is_none(){
                    let value=match fs::read_to_string(home.join(format!("{key}.json"))){
                        Ok(text)=>serde_json::from_str::<Value>(text.trim_start_matches('\u{feff}')).map_err(|_|io::Error::other(format!("invalid legacy {key} config")))?,
                        Err(e) if e.kind()==io::ErrorKind::NotFound=>if key=="subagent-policy"{json!({"enabled":false})}else{json!({})},Err(e)=>return Err(e),
                    };
                    let serialized=serde_json::to_string(&value).map_err(io::Error::other)?;table.insert(key,serialized.as_str()).map_err(io::Error::other)?;
                }
                table.insert(marker.as_str(),"true").map_err(io::Error::other)?;
            }
        }
        // Archive legacy files INSIDE redb, not loose credential JSON backups.
        for key in ["settings","models","auth","keybindings","subagent-policy","compaction-model"] {
            for suffix in ["json","json.migrated"] {
                let path=home.join(format!("{key}.{suffix}"));
                match fs::read_to_string(&path){
                    Ok(text)=>{
                        let backup=format!("legacy-backup/{key}.{suffix}");
                        let prior=table.get(backup.as_str()).map_err(io::Error::other)?.map(|v|v.value().to_owned());
                        if let Some(prior)=prior{
                            if prior!=text{let revision=format!("{backup}/{}",feather_core::message::fnv1a(text.as_bytes()));table.insert(revision.as_str(),text.as_str()).map_err(io::Error::other)?;}
                        }else{table.insert(backup.as_str(),text.as_str()).map_err(io::Error::other)?;}
                        cleanup.push((path,text));
                    },Err(e) if e.kind()==io::ErrorKind::NotFound=>{},Err(e)=>return Err(e),
                }
            }
        }
    }
    tx.commit().map_err(io::Error::other)?;
    // Files are removed only after the redb archival + active documents commit.
    // A corrupt source above aborts the transaction and is left untouched.
    for (path,archived) in cleanup {
        // Do not delete a legacy file an older process changed after archival.
        // Failure to clean a locked file must not block already-valid redb auth.
        if fs::read_to_string(&path).ok().as_deref()==Some(archived.as_str()){
            if let Err(error)=fs::remove_file(&path){if error.kind()!=io::ErrorKind::NotFound{eprintln!("feather: redb migration committed; legacy file cleanup deferred");}}
        }
    }
    f(&db)
}
pub fn read(home: &Path, key: &str) -> io::Result<Option<Value>> {
    with_db(home, |db| {
        let tx = db.begin_read().map_err(io::Error::other)?;
        let table = tx.open_table(DOCUMENTS).map_err(io::Error::other)?;
        let entry = table.get(key).map_err(io::Error::other)?;
        entry.map(|v| serde_json::from_str(v.value()).map_err(io::Error::other)).transpose()
    })
}
pub fn update(home: &Path, key: &str, edit: impl FnOnce(&mut Value)) -> io::Result<()> {
    with_db(home, |db| {
        let tx = db.begin_write().map_err(io::Error::other)?;
        {
            let mut table = tx.open_table(DOCUMENTS).map_err(io::Error::other)?;
            let mut value = match table.get(key).map_err(io::Error::other)? {
                Some(v) => serde_json::from_str(v.value()).map_err(io::Error::other)?,
                None => json!({}),
            };
            edit(&mut value);
            if key == "models" { strip_effort_presets(&mut value); }
            let text = serde_json::to_string(&value).map_err(io::Error::other)?;
            table.insert(key, text.as_str()).map_err(io::Error::other)?;
        }
        tx.commit().map_err(io::Error::other)
    })
}
pub fn write(home: &Path, key: &str, value: &Value) -> io::Result<()> {
    update(home, key, |v| *v = value.clone())
}
#[cfg(test)]
mod tests {
    #[test]
    fn stale_recreated_config_is_archived_verbatim_without_overriding_redb(){
        let home=std::env::temp_dir().join(feather_core::provider::gen_id("redb-rearchive"));fs::create_dir_all(&home).unwrap();
        fs::write(home.join("settings.json"),r#"{"model":"first"}"#).unwrap();read(&home,"settings").unwrap();
        update(&home,"settings",|v|v["model"]=json!("current")).unwrap();
        let stale=r#"{"model":"stale","unique":true}"#;fs::write(home.join("settings.json"),stale).unwrap();
        assert_eq!(read(&home,"settings").unwrap().unwrap()["model"],"current");assert!(!home.join("settings.json").exists());
        let archived=format!("legacy-backup/settings.json/{}",feather_core::message::fnv1a(stale.as_bytes()));assert_eq!(read(&home,&archived).unwrap().unwrap()["unique"],true);fs::remove_dir_all(home).unwrap();
    }
    #[test]
    fn oauth_and_preferences_migrate_to_redb_and_archive_without_loose_json(){
        let home=std::env::temp_dir().join(feather_core::provider::gen_id("redb-only-config"));fs::create_dir_all(&home).unwrap();
        fs::write(home.join("auth.json"),r#"{"synthetic":{"type":"api_key","key":"test-secret"}}"#).unwrap();
        fs::write(home.join("keybindings.json"),r#"{"app.interrupt":"escape"}"#).unwrap();
        fs::write(home.join("settings.json"),r#"{"model":"synthetic","unknown":42}"#).unwrap();
        assert_eq!(read(&home,"auth").unwrap().unwrap()["synthetic"]["key"],"test-secret");assert_eq!(read(&home,"settings").unwrap().unwrap()["unknown"],42);
        assert_eq!(read(&home,"keybindings").unwrap().unwrap()["app.interrupt"],"escape");
        for file in ["auth.json","settings.json","keybindings.json","auth.json.migrated"]{assert!(!home.join(file).exists());}
        assert!(read(&home,"legacy-backup/auth.json").unwrap().is_some(),"legacy backup is archived inside redb");
        let db=feather_core::database::create(home.join("config.redb")).unwrap();let tx=db.begin_read().unwrap();let table=tx.open_table(DOCUMENTS).unwrap();assert!(table.get("legacy-backup/auth.json").unwrap().is_some());drop(table);drop(tx);drop(db);fs::remove_dir_all(home).unwrap();
    }
    use super::*;
    #[test]
    fn migration_is_once_and_updates_preserve_unknown_fields() {
        let home = std::env::temp_dir().join(format!("feather-config-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        fs::create_dir_all(&home).unwrap();
        fs::write(home.join("settings.json"), r#"{"model":"old","unknown":42}"#).unwrap();
        fs::write(home.join("models.json"), r#"{"providers":{}}"#).unwrap();
        assert_eq!(read(&home, "settings").unwrap().unwrap()["model"], "old");
        update(&home, "settings", |v| v["model"] = json!("new")).unwrap();
        fs::write(home.join("settings.json"), r#"{"model":"stale"}"#).unwrap();
        let v = read(&home, "settings").unwrap().unwrap();
        assert_eq!(v["model"], "new");
        assert_eq!(v["unknown"], 42);
        fs::remove_dir_all(home).unwrap();
    }
    #[test]
    fn effort_presets_are_removed_without_deleting_routing_or_fetch_cache() {
        let home=std::env::temp_dir().join(feather_core::provider::gen_id("dynamic-config"));
        fs::create_dir_all(&home).unwrap();
        fs::write(home.join("models.json"),r#"{"providers":{"custom":{"baseUrl":"https://example.test/v1","apiKey":"synthetic","compat":{"supportsReasoningEffort":true,"thinkingFormat":"deepseek"},"modelOverrides":{"m":{"thinkingLevelMap":{"max":"max"}}},"models":[{"id":"m","reasoning":true,"thinkingLevels":["high"],"maxTokens":8192,"thinkingLevelMap":{"high":"high"}}]}}}"#).unwrap();
        let models=read(&home,"models").unwrap().unwrap();
        let p=&models["providers"]["custom"];
        assert_eq!(p["apiKey"],"synthetic");assert_eq!(p["baseUrl"],"https://example.test/v1");
        assert_eq!(p["compat"]["thinkingFormat"],"deepseek");
        assert!(p["compat"].get("supportsReasoningEffort").is_none());
        assert!(p["models"][0].get("reasoning").is_none());assert!(p["models"][0].get("thinkingLevels").is_none());
        assert_eq!(p["models"][0]["maxTokens"],8192);
        assert!(p["modelOverrides"]["m"].get("thinkingLevelMap").is_none());
        write(&home,"capabilities",&json!({"test":{"fetched":true}})).unwrap();
        read(&home,"models").unwrap();assert_eq!(read(&home,"capabilities").unwrap().unwrap()["test"]["fetched"],true);
        fs::remove_dir_all(home).unwrap();
    }
}
