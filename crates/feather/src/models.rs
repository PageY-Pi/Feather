//! Feather model catalog compatible with common provider metadata.
//!
//! `~/.feather/config.redb` stores provider and model metadata. The catalog supplies
//! contextWindow, maxTokens, pricing, reasoning support, provider baseUrl/apiKey,
//! and the optional compaction-model choice.
//! Builtin metadata is generated from `@earendil-works/pi-ai` 1.0.4 static
//! chat model catalog; it intentionally excludes user auth/settings/secrets.

use crate::config;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelsFile {
    pub providers: BTreeMap<String, ProviderEntry>,
}

#[derive(Debug, Clone, Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderEntry {
    #[serde(default)]
    pub base_url: Option<String>,
    /// "openai-completions" | "anthropic-messages" provider API names
    #[serde(default)]
    pub api: Option<String>,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub models: Vec<ModelEntry>,
    /// Static provider headers (for providers such as Copilot). Values must not
    /// contain secrets; dynamic credentials are handled by the caller.
    #[serde(default)]
    pub headers: Option<serde_json::Value>,
    /// Optional pi-style compatibility metadata. Deserialization only: the
    /// parent passes it to the provider builder (`with_compat`).
    #[serde(default)]
    pub compat: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCost {
    #[serde(default)]
    pub input: Option<f64>,
    #[serde(default)]
    pub output: Option<f64>,
    #[serde(default)]
    pub cache_read: Option<f64>,
    #[serde(default)]
    pub cache_write: Option<f64>,
}

#[derive(Debug, Clone, Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelEntry {
    pub id: String,
    /// Optional per-model wire API override. Providers such as Copilot expose
    /// OpenAI Responses, Anthropic Messages, and other protocols under one
    /// auth provider.
    #[serde(default)]
    pub api: Option<String>,
    /// Optional per-model base URL override. Some providers expose multiple
    /// wire APIs with different URL roots under one auth provider.
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub context_window: Option<u64>,
    #[serde(default)]
    pub max_tokens: Option<u64>,
    #[serde(default)]
    pub reasoning: bool,
    /// Explicit opt-out; absent means the standard adjustable effort list.
    #[serde(default)]
    pub reasoning_not_supported: bool,
    #[serde(default)]
    pub input: Vec<String>,
    #[serde(default)]
    pub cost: Option<ModelCost>,
    #[serde(default)]
    pub thinking_levels: Option<Vec<String>>,
    /// pi's model-level `thinkingLevelMap` (level -> effort string | null).
    /// Kept raw so the parent can hand it to the provider builder, which accepts
    /// the map at the top level or nested inside `compat`.
    #[serde(default)]
    pub thinking_level_map: Option<serde_json::Value>,
    /// Static model headers (for providers such as Copilot). Values must not
    /// contain secrets; dynamic credentials are handled by the caller.
    #[serde(default)]
    pub headers: Option<serde_json::Value>,
    /// Optional pi-style compatibility metadata. Deserialization only: the
    /// parent may merge it over the provider's `compat` before calling the
    /// provider builder.
    #[serde(default)]
    pub compat: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionModel {
    pub provider: String,
    pub id: String,
}

fn pi_agent_dir() -> Option<PathBuf> {
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .ok()?;
    Some(PathBuf::from(home).join(".pi").join("agent"))
}

pub fn catalog_path() -> PathBuf {
    config::feather_home().join("config.redb")
}
/// redb document id for the compaction-model choice.
pub const COMPACTION_DOC: &str = "compaction-model";
/// Feather reads its own model configuration; importing another agent's catalog is explicit.
pub fn load_compaction_model() -> Option<CompactionModel> {
    let home = config::feather_home();
    crate::config_store::read(&home, COMPACTION_DOC)
        .ok()
        .flatten()
        .and_then(|value| serde_json::from_value(value).ok())
}

/// Persist the compaction-model choice as a redb document (no JSON at runtime).
fn write_compaction_model(home: &std::path::Path, cm: &CompactionModel) -> std::io::Result<()> {
    crate::config_store::write(
        home,
        COMPACTION_DOC,
        &serde_json::json!({"provider": cm.provider, "id": cm.id}),
    )
}

pub const DEFAULT_EFFORTS: [&str; 6] = ["off", "low", "medium", "high", "xhigh", "max"];
pub fn thinking_levels(model: &ModelEntry, _provider: &ProviderEntry) -> Vec<String> {
    if model.reasoning_not_supported {
        vec!["off".into()]
    } else {
        DEFAULT_EFFORTS.iter().map(|s| s.to_string()).collect()
    }
}
fn apply_manual_effort(catalog: &mut ModelsFile) {
    for p in catalog.providers.values_mut() {
        if let Some(v) = p.compat.as_mut() {
            crate::config_store::strip_effort_presets(v);
        }
        for m in &mut p.models {
            if let Some(v) = m.compat.as_mut() {
                crate::config_store::strip_effort_presets(v);
            }
            m.reasoning = !m.reasoning_not_supported;
            let levels = if m.reasoning_not_supported {
                vec!["off".into()]
            } else {
                DEFAULT_EFFORTS
                    .iter()
                    .map(|s| s.to_string())
                    .collect::<Vec<_>>()
            };
            let map = feather_core::provider::compat::THINKING_LEVELS
                .iter()
                .map(|l| {
                    (
                        (*l).to_string(),
                        if levels.iter().any(|v| v == l) {
                            serde_json::json!(l)
                        } else {
                            serde_json::Value::Null
                        },
                    )
                })
                .collect::<serde_json::Map<_, _>>();
            m.thinking_levels = Some(levels);
            m.thinking_level_map = Some(serde_json::Value::Object(map));
        }
    }
}

pub fn clamp_thinking(
    model: &ModelEntry,
    provider: &ProviderEntry,
    requested: feather_core::provider::ThinkingLevel,
) -> feather_core::provider::ThinkingLevel {
    let supported = thinking_levels(model, provider);
    let order = ["off", "minimal", "low", "medium", "high", "xhigh", "max"];
    let position = order
        .iter()
        .position(|s| *s == requested.as_str())
        .unwrap_or(0);
    let chosen = order[position..]
        .iter()
        .chain(order[..position].iter().rev())
        .find(|s| supported.iter().any(|v| v == **s))
        .copied()
        .unwrap_or("off");
    feather_core::provider::ThinkingLevel::parse(chosen)
        .unwrap_or(feather_core::provider::ThinkingLevel::Off)
}
fn builtin_catalog() -> Option<&'static ModelsFile> {
    // The builtin catalog is baked in at compile time and immutable: parse the
    // 434 KB JSON once per process instead of on every `load()` (it is hit per
    // transcript entry in RPC `get_entries`).
    static CACHE: std::sync::OnceLock<Option<ModelsFile>> = std::sync::OnceLock::new();
    CACHE
        .get_or_init(|| serde_json::from_str(include_str!("builtin-models.json")).ok())
        .as_ref()
}

fn merge_catalogs(builtin: &ModelsFile, user: ModelsFile) -> ModelsFile {
    let mut merged = ModelsFile {
        providers: builtin
            .providers
            .iter()
            .filter(|(id, _)| !user.providers.contains_key(*id))
            .map(|(id, p)| (id.clone(), p.clone()))
            .collect(),
    };
    for (id, mut user_provider) in user.providers {
        if let Some(builtin_provider) = builtin.providers.get(&id) {
            if user_provider.base_url.is_none() {
                user_provider.base_url = builtin_provider.base_url.clone();
            }
            if user_provider.api.is_none() {
                user_provider.api = builtin_provider.api.clone();
            }
            if user_provider.api_key.is_none() {
                user_provider.api_key = builtin_provider.api_key.clone();
            }
            if user_provider.headers.is_none() {
                user_provider.headers = builtin_provider.headers.clone();
            }
            if user_provider.compat.is_none() {
                user_provider.compat = builtin_provider.compat.clone();
            }
        }
        // User provider entries, including their model arrays, win wholesale.
        merged.providers.insert(id, user_provider);
    }
    merged
}

pub fn load() -> Option<ModelsFile> {
    load_for(&config::feather_home())
}
pub fn load_for(home: &std::path::Path) -> Option<ModelsFile> {
    let mut value = crate::config_store::read(home, "models").ok().flatten()?;
    let deleted = value
        .get_mut("deletedModels")
        .map(serde_json::Value::take)
        .unwrap_or_default();
    let user = serde_json::from_value::<ModelsFile>(value).ok();
    let mut catalog = match (builtin_catalog(), user) {
        (Some(b), Some(u)) => merge_catalogs(b, u),
        (Some(b), None) => b.clone(),
        (None, Some(u)) => u,
        _ => return None,
    };
    if let Some(deleted) = deleted.as_array() {
        for item in deleted.iter().filter_map(|v| v.as_str()) {
            if let Some((pid, id)) = item.split_once('/') {
                if let Some(p) = catalog.providers.get_mut(pid) {
                    p.models.retain(|m| m.id != id);
                }
            }
        }
    }
    apply_manual_effort(&mut catalog);
    Some(catalog)
}

/// Persisted models only, with routing defaults resolved. Do not fetch the
/// thousands of bundled models merely because a provider credential exists.
pub fn saved_for(home: &std::path::Path) -> std::io::Result<ModelsFile> {
    let value = crate::config_store::read(home, "models")?.unwrap_or_else(|| serde_json::json!({}));
    let all = load_for(home).ok_or_else(|| std::io::Error::other("model catalog unavailable"))?;
    let mut providers = BTreeMap::new();
    if let Some(saved) = value["providers"].as_object() {
        for (pid, entry) in saved {
            if let Some(p) = all.providers.get(pid) {
                let ids: std::collections::BTreeSet<&str> = entry["models"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|m| m["id"].as_str())
                    .collect();
                let mut p = p.clone();
                p.models.retain(|m| ids.contains(m.id.as_str()));
                if !p.models.is_empty() {
                    providers.insert(pid.clone(), p);
                }
            }
        }
    }
    Ok(ModelsFile { providers })
}
pub fn save(catalog: &ModelsFile) -> std::io::Result<()> {
    let value = serde_json::to_value(catalog).map_err(std::io::Error::other)?;
    crate::config_store::update(&config::feather_home(), "models", |v| {
        v["providers"] = value["providers"].clone();
    })
}
pub fn upsert_model(pid: &str, provider: ProviderEntry, model: ModelEntry) -> std::io::Result<()> {
    let pv = serde_json::to_value(&provider).map_err(std::io::Error::other)?;
    let mv = serde_json::to_value(&model).map_err(std::io::Error::other)?;
    crate::config_store::update(&config::feather_home(), "models", |v| {
        if !v["providers"].is_object() {
            v["providers"] = serde_json::json!({});
        }
        if !v["providers"][pid].is_object() {
            v["providers"][pid] = pv.clone();
        }
        let p = &mut v["providers"][pid];
        p["api"] = pv["api"].clone();
        p["baseUrl"] = pv["baseUrl"].clone();
        p["apiKey"] = pv["apiKey"].clone();
        if !p["models"].is_array() {
            p["models"] = serde_json::json!([]);
        }
        let list = p["models"].as_array_mut().unwrap();
        list.retain(|m| m["id"].as_str() != Some(&model.id));
        list.push(mv);
        if let Some(d) = v["deletedModels"].as_array_mut() {
            d.retain(|i| i.as_str() != Some(&format!("{pid}/{}", model.id)));
        }
    })
}
pub fn delete_model(pid: &str, id: &str) -> std::io::Result<()> {
    delete_model_for(&config::feather_home(), pid, id)
}
pub fn delete_model_for(home: &std::path::Path, pid: &str, id: &str) -> std::io::Result<()> {
    crate::config_store::update(home, "models", |v| {
        if let Some(list) = v
            .get_mut("providers")
            .and_then(|p| p.get_mut(pid))
            .and_then(|p| p.get_mut("models"))
            .and_then(serde_json::Value::as_array_mut)
        {
            list.retain(|m| m["id"].as_str() != Some(id));
        }
        if !v["deletedModels"].is_array() {
            v["deletedModels"] = serde_json::json!([]);
        }
        let key = serde_json::json!(format!("{pid}/{id}"));
        let d = v["deletedModels"].as_array_mut().unwrap();
        if !d.contains(&key) {
            d.push(key);
        }
    })
}
/// Look a model up: `"provider/id"` form wins, otherwise the first provider
/// declaring the id. Returns `(provider_id, provider, model)`.
pub fn find<'a>(
    cat: &'a ModelsFile,
    model: &'a str,
) -> Option<(&'a str, &'a ProviderEntry, &'a ModelEntry)> {
    if let Some((prov, id)) = model.split_once('/') {
        let p = cat.providers.get(prov)?;
        let m = p.models.iter().find(|m| m.id == id)?;
        return Some((prov, p, m));
    }
    for (pid, p) in &cat.providers {
        if let Some(m) = p.models.iter().find(|m| m.id == model) {
            return Some((pid.as_str(), p, m));
        }
    }
    None
}

/// Sync pi's catalog into FEATHER_HOME and return pi's compaction-model
/// choice. Idempotent; a missing pi install is a no-op.
pub fn sync_from_pi() -> Option<CompactionModel> {
    let pi = pi_agent_dir()?;
    let src = pi.join("models.json");
    if src.exists() {
        if let Ok(text) = std::fs::read_to_string(&src) {
            if let Ok(catalog) = serde_json::from_str::<ModelsFile>(&text) {
                let _ = save(&catalog);
            }
        }
    }
    let cm = std::fs::read_to_string(pi.join("compaction-model.json")).ok()?;
    let cm: CompactionModel = serde_json::from_str(&cm).ok()?;
    // Pi remains an explicit one-time import source; runtime reads only redb.
    let _ = write_compaction_model(&config::feather_home(), &cm);
    Some(cm)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persisted_model_deletions_survive_reload_including_builtins() {
        let home = std::env::temp_dir().join(feather_core::provider::gen_id("model-delete"));
        let builtin = builtin_catalog().unwrap();
        let (pid, p) = builtin
            .providers
            .iter()
            .find(|(_, p)| !p.models.is_empty())
            .unwrap();
        let id = &p.models[0].id;
        assert!(load_for(&home).unwrap().providers[pid]
            .models
            .iter()
            .any(|m| &m.id == id));
        delete_model_for(&home, pid, id).unwrap();
        for _ in 0..2 {
            assert!(!load_for(&home).unwrap().providers[pid]
                .models
                .iter()
                .any(|m| &m.id == id));
        }
        crate::config_store::write(
            &home,
            "models",
            &serde_json::json!({"providers":{"custom":{"models":[{"id":"one"},{"id":"two"}]}}}),
        )
        .unwrap();
        delete_model_for(&home, "custom", "one").unwrap();
        let cat = load_for(&home).unwrap();
        assert_eq!(cat.providers["custom"].models.len(), 1);
        assert_eq!(cat.providers["custom"].models[0].id, "two");
        std::fs::remove_dir_all(home).unwrap();
    }
    #[test]
    fn catalog_entry_keeps_pi_thinking_map_and_compat() {
        let txt = r#"{
          "providers": {
            "anthropic": {
              "api": "anthropic-messages",
              "compat": { "supportsTemperature": true },
              "models": [{
                "id": "claude-opus-4-6",
                "reasoning": true,
                "maxTokens": 128000,
                "thinkingLevelMap": { "max": "max" },
                "compat": { "forceAdaptiveThinking": true }
              }]
            }
          }
        }"#;
        let cat: ModelsFile = serde_json::from_str(txt).unwrap();
        let provider = cat.providers.get("anthropic").unwrap();
        assert_eq!(
            provider.compat.as_ref().unwrap()["supportsTemperature"],
            serde_json::json!(true)
        );
        let model = &provider.models[0];
        assert_eq!(model.max_tokens, Some(128000));
        assert_eq!(
            model.thinking_level_map.as_ref().unwrap()["max"],
            serde_json::json!("max")
        );
        assert_eq!(
            model.compat.as_ref().unwrap()["forceAdaptiveThinking"],
            serde_json::json!(true)
        );
    }

    #[test]
    fn builtin_zai_and_codex_are_available() {
        let cat = builtin_catalog().unwrap();
        let zai = cat.providers.get("zai").unwrap();
        assert_eq!(zai.api.as_deref(), Some("openai-completions"));
        assert!(zai.models.iter().any(|m| m.id.starts_with("glm-")));

        let codex = cat.providers.get("openai-codex").unwrap();
        assert_eq!(codex.api.as_deref(), Some("openai-codex-responses"));
        assert!(codex
            .models
            .iter()
            .any(|m| m.api.as_deref() == Some("openai-codex-responses")));
    }

    #[test]
    fn builtin_model_api_overrides_are_preserved() {
        let cat = builtin_catalog().unwrap();
        let copilot = cat.providers.get("github-copilot").unwrap();
        let mut apis: Vec<_> = copilot
            .models
            .iter()
            .filter_map(|m| m.api.as_deref())
            .collect();
        apis.sort_unstable();
        apis.dedup();
        assert!(
            apis.len() > 1,
            "copilot catalog should carry per-model APIs"
        );
        assert!(apis.contains(&"anthropic-messages"));
        assert!(apis.contains(&"openai-responses"));
    }

    #[test]
    fn builtin_openrouter_preserves_per_model_base_urls() {
        let cat = builtin_catalog().unwrap();
        let openrouter = cat.providers.get("openrouter").unwrap();
        assert_eq!(
            openrouter.base_url.as_deref(),
            Some("https://openrouter.ai/api")
        );

        let anthropic = openrouter
            .models
            .iter()
            .find(|m| m.api.as_deref() == Some("anthropic-messages"))
            .unwrap();
        assert_eq!(
            anthropic.base_url.as_deref(),
            Some("https://openrouter.ai/api")
        );

        let completions = openrouter
            .models
            .iter()
            .find(|m| m.api.as_deref() == Some("openai-completions"))
            .unwrap();
        assert_eq!(
            completions.base_url.as_deref(),
            Some("https://openrouter.ai/api/v1")
        );
    }

    #[test]
    fn merge_preserves_custom_provider_and_fills_builtin_defaults() {
        let builtin: ModelsFile = serde_json::from_str(
            r#"{
          "providers": {
            "zai": {
              "baseUrl": "https://api.z.ai/api/coding/paas/v4",
              "api": "openai-completions",
              "apiKey": "$ZAI_API_KEY",
              "models": [{ "id": "glm-5.3", "api": "openai-completions" }]
            }
          }
        }"#,
        )
        .unwrap();
        let user: ModelsFile = serde_json::from_str(r#"{
          "providers": {
            "zai": { "models": [{ "id": "custom-glm", "api": "anthropic-messages" }] },
            "local": { "baseUrl": "http://localhost:11434/v1", "api": "openai-completions", "models": [{ "id": "qwen" }] }
          }
        }"#).unwrap();

        let merged = merge_catalogs(&builtin, user);
        let zai = merged.providers.get("zai").unwrap();
        assert_eq!(
            zai.base_url.as_deref(),
            Some("https://api.z.ai/api/coding/paas/v4")
        );
        assert_eq!(zai.api.as_deref(), Some("openai-completions"));
        assert_eq!(zai.api_key.as_deref(), Some("$ZAI_API_KEY"));
        assert_eq!(zai.models.len(), 1);
        assert_eq!(zai.models[0].id, "custom-glm");
        assert_eq!(zai.models[0].api.as_deref(), Some("anthropic-messages"));
        assert!(merged.providers.contains_key("local"));
    }
}
