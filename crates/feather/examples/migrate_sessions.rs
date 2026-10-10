//! Offline migration utility: close Feather windows before running.
//! Archives old source files only after successful import/readback.
fn main() -> anyhow::Result<()> {
    let home = feather::config::feather_home();
    let mut count = 0;
    let entries = match std::fs::read_dir(home.join("sessions")) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    for entry in entries {
        let dir = entry?.path();
        if !dir.is_dir() {
            continue;
        }
        if !dir.join("session.jsonl").exists() && !dir.join("session.redb").exists() {
            continue;
        }
        let session = feather_core::Session::open(&dir, &home)?;
        let _tasks = feather::tasks::TaskStore::try_open(&dir).map_err(anyhow::Error::msg)?;
        let records = feather_core::records::Records::open(&dir.join("checkpoints.redb"))
            .map_err(anyhow::Error::msg)?;
        let legacy = dir.join("turn_commits.json");
        if legacy.exists() {
            let value: serde_json::Value = serde_json::from_slice(&std::fs::read(&legacy)?)?;
            records.put("turns", &value).map_err(anyhow::Error::msg)?;
            let verified: serde_json::Value = records
                .get("turns")
                .map_err(anyhow::Error::msg)?
                .ok_or_else(|| anyhow::anyhow!("checkpoint import missing"))?;
            anyhow::ensure!(verified == value, "checkpoint verification mismatch");
            std::fs::rename(&legacy, legacy.with_extension("json.legacy"))?;
        }
        let legacy = dir.join("session-name.json");
        if legacy.exists() {
            let value: serde_json::Value = serde_json::from_slice(&std::fs::read(&legacy)?)?;
            let name = value["name"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("invalid name"))?;
            feather_core::records::Records::open(&dir.join("metadata.redb"))
                .and_then(|store| store.put("name", &name))
                .map_err(anyhow::Error::msg)?;
            std::fs::rename(&legacy, legacy.with_extension("json.legacy"))?;
        }
        migrate_anchors(&dir)?;
        println!(
            "Migrated {}: {} audit messages",
            session.id(),
            session.raw_messages()?.len()
        );
        count += 1;
    }
    println!("Verified {count} sessions; legacy backups retained.");
    Ok(())
}
fn migrate_anchors(root: &std::path::Path) -> anyhow::Result<()> {
    for entry in std::fs::read_dir(root)? {
        let path = entry?.path();
        if path.is_dir() {
            migrate_anchors(&path)?;
        } else if path.extension().is_some_and(|e| e == "json")
            && path
                .parent()
                .and_then(|p| p.file_name())
                .is_some_and(|n| n == "anchors")
        {
            let rec: feather_hashline::engine::PersistRecord =
                serde_json::from_slice(&std::fs::read(&path)?)?;
            let home = path
                .parent()
                .and_then(|p| p.parent())
                .ok_or_else(|| anyhow::anyhow!("anchor home missing"))?;
            let key = std::fs::canonicalize(&rec.path)
                .unwrap_or_else(|_| std::path::PathBuf::from(&rec.path))
                .display()
                .to_string();
            let store = feather_core::records::Records::open(&home.join("anchors.redb"))
                .map_err(anyhow::Error::msg)?;
            store.put(&key, &rec).map_err(anyhow::Error::msg)?;
            let verified: feather_hashline::engine::PersistRecord = store
                .get(&key)
                .map_err(anyhow::Error::msg)?
                .ok_or_else(|| anyhow::anyhow!("anchor import missing"))?;
            anyhow::ensure!(
                serde_json::to_vec(&verified)? == serde_json::to_vec(&rec)?,
                "anchor verification mismatch"
            );
            std::fs::rename(&path, path.with_extension("json.legacy"))?;
        }
    }
    Ok(())
}
