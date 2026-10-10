//! Explicit recovery of a legacy model backup into config.redb. No secrets are printed.
//! cargo run -p feather --example import_models_backup --release -- [--apply] [--home PATH]
use feather::{config, config_store, models::ModelsFile};
use serde_json::{json, Value};
use std::{collections::BTreeSet, io, path::PathBuf};

#[derive(Default, Debug)]
struct Summary {
    providers_added: usize,
    models_added: usize,
    fields_recovered: usize,
    conflicts_preserved: usize,
    deleted_skipped: usize,
}
fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
fn validate(value: &Value) -> io::Result<()> {
    serde_json::from_value::<ModelsFile>(value.clone())
        .map_err(|_| invalid("invalid model catalog schema"))?;
    let providers = value["providers"]
        .as_object()
        .ok_or_else(|| invalid("providers must be an object"))?;
    for (pid, provider) in providers {
        if pid.trim().is_empty() {
            return Err(invalid("empty provider ID"));
        }
        let mut ids = BTreeSet::new();
        if let Some(models) = provider.get("models") {
            let models = models
                .as_array()
                .ok_or_else(|| invalid("models must be an array"))?;
            for model in models {
                let id = model["id"]
                    .as_str()
                    .filter(|id| !id.trim().is_empty())
                    .ok_or_else(|| invalid("model ID missing or empty"))?;
                if !ids.insert(id) {
                    return Err(invalid("duplicate model ID within provider"));
                }
            }
        }
    }
    Ok(())
}
fn recover_fields(dst: &mut Value, src: &Value, summary: &mut Summary) {
    if let (Some(dst), Some(src)) = (dst.as_object_mut(), src.as_object()) {
        for (key, value) in src {
            match dst.get_mut(key) {
                None => {
                    dst.insert(key.clone(), value.clone());
                    summary.fields_recovered += 1;
                }
                Some(old) if old.is_null() && !value.is_null() => {
                    *old = value.clone();
                    summary.fields_recovered += 1;
                }
                Some(old) if old.is_object() && value.is_object() => {
                    recover_fields(old, value, summary)
                }
                Some(old) if old != value => summary.conflicts_preserved += 1,
                _ => {}
            }
        }
    }
}
fn merge(dst: &mut Value, src: &Value) -> io::Result<Summary> {
    validate(src)?;
    if !dst.is_object() {
        return Err(invalid("stored models document must be an object"));
    }
    if dst.get("providers").is_none() {
        dst["providers"] = json!({});
    }
    // Validate before mutation, including existing catalog and duplicate IDs.
    validate(dst)?;
    let deleted: BTreeSet<String> = dst["deletedModels"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str().map(str::to_owned))
        .collect();
    let mut summary = Summary::default();
    for (pid, provider) in src["providers"].as_object().unwrap() {
        if dst["providers"].get(pid).is_none() {
            dst["providers"][pid] = json!({});
            summary.providers_added += 1;
        }
        let target = &mut dst["providers"][pid];
        let mut fields = provider.clone();
        fields.as_object_mut().unwrap().remove("models");
        recover_fields(target, &fields, &mut summary);
        if target.get("models").is_none() {
            target["models"] = json!([]);
        }
        let list = target["models"].as_array_mut().unwrap();
        for model in provider["models"].as_array().into_iter().flatten() {
            let id = model["id"].as_str().unwrap();
            if deleted.contains(&format!("{pid}/{id}")) {
                summary.deleted_skipped += 1;
                continue;
            }
            if let Some(existing) = list.iter_mut().find(|v| v["id"].as_str() == Some(id)) {
                recover_fields(existing, model, &mut summary);
            } else {
                list.push(model.clone());
                summary.models_added += 1;
            }
        }
    }
    // Preserve opaque extension metadata without typed serialization dropping it.
    let mut fields = src.clone();
    fields.as_object_mut().unwrap().remove("providers");
    fields.as_object_mut().unwrap().remove("deletedModels");
    recover_fields(dst, &fields, &mut summary);
    validate(dst)?;
    Ok(summary)
}
fn run() -> io::Result<()> {
    let mut home = config::feather_home();
    let mut apply = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--apply" => apply = true,
            "--home" => {
                home = PathBuf::from(args.next().ok_or_else(|| invalid("--home needs a path"))?)
            }
            _ => {
                return Err(invalid(
                    "usage: import_models_backup [--apply] [--home PATH]",
                ))
            }
        }
    }
    let source = home.join("models.json.migrated");
    let bytes = std::fs::read(&source)?;
    let text = std::str::from_utf8(&bytes).map_err(|_| invalid("model backup is not UTF-8"))?;
    let value: Value = serde_json::from_str(text.trim_start_matches('\u{feff}'))
        .map_err(|_| invalid("model backup is invalid JSON"))?;
    validate(&value)?;
    // Refuse automatic first-use migration as part of this explicit recovery.
    if !home.join("config.redb").is_file() {
        return Err(invalid(
            "config.redb must already exist; initialize Feather first",
        ));
    }
    let before = config_store::read(&home, "models")?
        .ok_or_else(|| invalid("stored models document missing"))?;
    let settings = config_store::read(&home, "settings")?;
    let mut preview = before.clone();
    let summary = merge(&mut preview, &value)?;
    println!(
        "{}: {:?}",
        if apply { "Import plan" } else { "Dry run" },
        summary
    );
    if !apply {
        return Ok(());
    }
    let mut expected = None;
    let mut failure = None;
    config_store::update(&home, "models", |document| {
        let mut candidate = document.clone();
        match merge(&mut candidate, &value) {
            Ok(_) => {
                expected = Some(candidate.clone());
                *document = candidate;
            }
            Err(error) => failure = Some(error),
        }
    })?;
    if let Some(error) = failure {
        return Err(error);
    }
    let readback =
        config_store::read(&home, "models")?.ok_or_else(|| invalid("readback missing"))?;
    if Some(&readback) != expected.as_ref() {
        return Err(invalid("full model readback verification failed"));
    }
    if config_store::read(&home, "settings")? != settings {
        return Err(invalid("settings changed during recovery"));
    }
    if std::fs::read(source)? != bytes {
        return Err(invalid("source backup changed during recovery"));
    }
    let mut repeated = readback.clone();
    merge(&mut repeated, &value)?;
    if repeated != readback {
        return Err(invalid("recovery is not idempotent"));
    }
    let catalog =
        feather::models::load_for(&home).ok_or_else(|| invalid("runtime catalog reload failed"))?;
    let providers = value["providers"].as_object().unwrap();
    let mut verified_models = 0;
    for (pid, provider) in providers {
        for model in provider["models"].as_array().into_iter().flatten() {
            let model_id = model["id"].as_str().unwrap();
            if readback["deletedModels"].as_array().is_some_and(|list| {
                list.iter()
                    .any(|v| v.as_str() == Some(&format!("{pid}/{model_id}")))
            }) {
                continue;
            }
            if !catalog
                .providers
                .get(pid)
                .is_some_and(|p| p.models.iter().any(|m| m.id == model_id))
            {
                return Err(invalid("imported model missing from runtime catalog"));
            }
            verified_models += 1;
        }
    }
    println!(
        "Runtime catalog verified: {} source providers / {verified_models} models",
        providers.len()
    );
    println!("Verified: complete model document, settings unchanged, source unchanged, idempotent re-import. Existing values and deletions take precedence.");
    Ok(())
}
fn main() {
    if let Err(error) = run() {
        eprintln!("Model recovery failed: {error}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recover_missing_preserve_edits_credentials_deletions_and_unknowns() {
        let src = json!({"providers":{"custom":{"apiKey":"backup-key","baseUrl":"https://backup.invalid/v1","api":"openai-completions","compat":{"newField":true},"models":[{"id":"first","maxTokens":1234,"opaque":{"x":1}},{"id":"second"},{"id":"deleted"}]}},"unknownRoot":true});
        let mut dst = json!({"providers":{"custom":{"apiKey":"current-key","baseUrl":"https://current.invalid/v1","models":[{"id":"first","name":"Edited","maxTokens":500}],"keep":true}},"deletedModels":["custom/deleted"]});
        let summary = merge(&mut dst, &src).unwrap();
        assert_eq!(summary.models_added, 1);
        assert_eq!(summary.deleted_skipped, 1);
        assert_eq!(dst["providers"]["custom"]["apiKey"], "current-key");
        assert_eq!(
            dst["providers"]["custom"]["baseUrl"],
            "https://current.invalid/v1"
        );
        assert_eq!(dst["providers"]["custom"]["models"][0]["maxTokens"], 500);
        assert_eq!(dst["providers"]["custom"]["models"][0]["opaque"]["x"], 1);
        let stable = dst.clone();
        merge(&mut dst, &src).unwrap();
        assert_eq!(dst, stable);
    }
    #[test]
    fn malformed_source_rejected_before_merge() {
        let mut dst = json!({});
        let source = json!({"providers":{"p":{"models":[{"id":"same"},{"id":"same"}]}}});
        assert!(merge(&mut dst, &source).is_err());
        assert_eq!(dst, json!({}));
    }
    #[test]
    fn atomic_config_roundtrip_preserves_non_model_documents() {
        let home = std::env::temp_dir().join(feather_core::provider::gen_id("model-recovery-test"));
        config_store::write(&home, "settings", &json!({"model":"selected","unknown":42})).unwrap();
        let source = json!({"providers":{"p":{"api":"openai-completions","apiKey":"synthetic","models":[{"id":"model","thinkingLevels":["off","high"],"extension":17}]}}});
        config_store::update(&home, "models", |v| {
            merge(v, &source).unwrap();
        })
        .unwrap();
        assert_eq!(
            config_store::read(&home, "models").unwrap().unwrap(),
            source
        );
        assert_eq!(
            config_store::read(&home, "settings").unwrap().unwrap(),
            json!({"model":"selected","unknown":42})
        );
        std::fs::remove_dir_all(home).unwrap();
    }
}
