//! Feather model catalog: bundled defaults merged with user state.
//!
//! Two sources are resolved on every load:
//! * **Bundled** defaults compiled into the binary from `builtin-models.json`
//!   (`include_str!`), generated from the `@earendil-works/pi-ai` 1.0.4 static
//!   chat model catalog. They supply contextWindow, maxTokens, pricing,
//!   reasoning support and the provider baseUrl/api.
//! * **User** state in the `models` document of `~/.feather/config.redb`:
//!   credentials, custom providers, and per-model overrides.
//!
//! Built-in providers are Feather-owned, so their routing always comes from the
//! bundle and only the credential is user-supplied. The resolved catalog is
//! memoized (see `with_catalog`) because it is read per wire message.

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

#[derive(Debug, Clone, Default, Deserialize, serde::Serialize)]
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
    /// Model capability advertised by the provider: the largest context the
    /// server accepts. The *effective* window is the smaller of this and the
    /// global context setting (see [`effective_context_window`]).
    #[serde(default)]
    pub context_window: Option<u64>,
    /// Maximum output tokens the model accepts for one assistant turn.
    #[serde(rename = "maxTokens", alias = "max_tokens", default)]
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
/// Volcengine/Volcano account ids. `volcengine-plan` is the bundled Coding Plan
/// (API-key) provider; `volcano` and `volcengine` remain here because a config
/// written by an earlier build may still select them — they no longer ship a
/// bundled provider entry, so the compat overrides must follow the id alone.
pub fn is_volcengine_provider(provider: &str) -> bool {
    matches!(provider, "volcano" | "volcengine" | "volcengine-plan")
}

/// Providers whose model list ships with Feather: OAuth sign-in providers and
/// built-in API-key/Coding-Plan providers. Their models are owned by the
/// bundled catalog, so the custom editor must not add, edit or delete them -
/// only the credential (API key / OAuth token) is user-owned.
pub fn is_managed_provider(provider: &str) -> bool {
    builtin_catalog().is_some_and(|c| c.providers.contains_key(provider))
}

/// Reject mutations of a managed provider's models with a readable error.
fn guard_managed(provider: &str) -> std::io::Result<()> {
    if is_managed_provider(provider) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "{provider} is a built-in provider; its models are read-only. \
                 Connect it in 'OAuth / built-in providers' or add a provider with a different id."
            ),
        ));
    }
    Ok(())
}

/// Volcengine Coding Plan uses DeepSeek-style controls on its OpenAI endpoint:
/// a top-level `thinking` toggle plus `reasoning_effort` (`low`..`max`, verified
/// against Ark's coding endpoint).
///
/// The GLM-5.3 family is forced-thinking: Ark answers 400
/// `thinking.type 'disabled' is not supported by this model`, so its `off` maps
/// to `null` (unsupported) and is never sent. Every other coding-plan model is a
/// convertible reasoner and keeps the disable toggle.
pub fn volcengine_thinking_map(provider: &str, model: &str) -> Option<serde_json::Value> {
    if !is_volcengine_provider(provider) {
        return None;
    }
    // GLM-5.3/GLM-5.3-Flash: no `off`, and `minimal` is rejected as `none`.
    if model.starts_with("glm-5.3") {
        return Some(serde_json::json!({
            "off": null,
            "minimal": null,
            "low": "low",
            "medium": "medium",
            "high": "high",
            "xhigh": "xhigh",
            "max": "max"
        }));
    }
    // Convertible reasoners: `off` sends `thinking.type: "disabled"` (the mapped
    // string is only a "supported" marker for that level), and `minimal` stays
    // unmapped because Ark normalises it to `none`, which is not a real effort.
    Some(serde_json::json!({
        "off": "none",
        "minimal": null,
        "low": "low",
        "medium": "medium",
        "high": "high",
        "xhigh": "xhigh",
        "max": "max"
    }))
}
fn model_effort_levels(model: &ModelEntry) -> Option<Vec<String>> {
    if let Some(levels) = &model.thinking_levels {
        if !levels.is_empty() {
            return Some(levels.clone());
        }
    }
    let map = model
        .thinking_level_map
        .as_ref()
        .and_then(serde_json::Value::as_object)?;
    let map = map
        .iter()
        .map(|(level, value)| (level.clone(), value.clone()))
        .collect();
    let levels = feather_core::provider::compat::supported_levels(&map)
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>();
    (!levels.is_empty()).then_some(levels)
}
pub const DEFAULT_EFFORTS: [&str; 6] = ["off", "low", "medium", "high", "xhigh", "max"];
pub fn thinking_levels(model: &ModelEntry, _provider: &ProviderEntry) -> Vec<String> {
    if model.reasoning_not_supported {
        return vec!["off".into()];
    }
    if let Some(levels) = model_effort_levels(model) {
        return levels;
    }
    DEFAULT_EFFORTS.iter().map(|s| s.to_string()).collect()
}
fn apply_manual_effort(catalog: &mut ModelsFile) {
    for (pid, p) in catalog.providers.iter_mut() {
        if let Some(v) = p.compat.as_mut() {
            crate::config_store::strip_effort_presets(v);
        }
        for m in &mut p.models {
            // Volcano Coding Plan models have no per-model map in the bundled
            // catalog; derive it here unless the entry already carries one.
            if m.thinking_level_map.is_none() {
                if let Some(map) = volcengine_thinking_map(pid, &m.id) {
                    m.thinking_level_map = Some(map);
                }
            }
            if let Some(v) = m.compat.as_mut() {
                crate::config_store::strip_effort_presets(v);
            }
            if m.reasoning_not_supported {
                m.reasoning = false;
                m.thinking_levels = Some(vec!["off".into()]);
                m.thinking_level_map = Some(serde_json::json!({
                    "off": "off",
                    "minimal": null,
                    "low": null,
                    "medium": null,
                    "high": null,
                    "xhigh": null,
                    "max": null
                }));
                continue;
            }
            // Preserve explicit built-in model maps. They encode provider-specific
            // limits such as Volcano's xhigh -> high mapping and fixed-thinking
            // models whose `off` level is deliberately unsupported.
            if m.thinking_level_map.is_some() {
                m.reasoning = true;
                m.thinking_levels = model_effort_levels(m);
                continue;
            }
            m.reasoning = true;
            let levels = DEFAULT_EFFORTS
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>();
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
        // Borrowed here, cloned only in the one branch that needs the list.
        let builtin_models = builtin.providers.get(&id).map(|p| &p.models);
        if let Some(builtin_provider) = builtin.providers.get(&id) {
            // Built-in providers own their routing: a persisted entry contributes
            // its credential (API key / OAuth token) and user extras, but never a
            // conflicting endpoint. Without this, a stale or hand-edited
            // `api`/`baseUrl` sends every request to the wrong server.
            if is_managed_provider(&id) {
                // Built-in ids always route to their bundled endpoint; a persisted
                // value here is stale data, not a user preference.
                user_provider.base_url = builtin_provider.base_url.clone();
                user_provider.api = builtin_provider.api.clone();
            } else {
                if user_provider.base_url.is_none() {
                    user_provider.base_url = builtin_provider.base_url.clone();
                }
                if user_provider.api.is_none() {
                    user_provider.api = builtin_provider.api.clone();
                }
            }
            if user_provider.api_key.is_none() {
                user_provider.api_key = builtin_provider.api_key.clone();
            }
            if user_provider.headers.is_none() {
                user_provider.headers = builtin_provider.headers.clone();
            }
            // Bundled compat keys win per key; user-only keys (session affinity,
            // long cache retention, …) survive.
            user_provider.compat = Some(feather_core::provider::compat::merge_compat(
                user_provider.compat.as_ref(),
                builtin_provider.compat.as_ref(),
            ));
        }
        if user_provider.models.is_empty() && is_managed_provider(&id) {
            if let Some(builtin_models) = builtin_models.filter(|m| !m.is_empty()) {
                // Built-in providers may be persisted before their model list is
                // imported (an OAuth/API-key login writes the credential entry
                // first). Keep the credential but expose the bundled model
                // definitions.
                user_provider.models = builtin_models.clone();
            }
        }
        // User provider entries, including their model arrays, win wholesale.
        merged.providers.insert(id, user_provider);
    }
    merged
}

/// Cache key for the resolved catalog. Rebuilding parses the persisted `models`
/// document, deep-merges the ~434 KB bundled catalog and re-derives every
/// model's effort levels, so the result is memoized under this key instead of
/// being repeated per wire message.
///
/// The generation counter is bumped by every write path (see
/// `config_store::update`); the size+mtime stamp additionally catches an
/// external writer such as another Feather process. Both checks are O(1).
#[derive(Clone, PartialEq, Eq)]
struct CatalogKey {
    generation: u64,
    stamp: Option<(u64, u64)>,
}

/// `(key, resolved catalog)`. A `None` catalog means "resolved and empty"; it is
/// cached too, so a missing catalog does not force a rebuild on every lookup.
type CatalogCache = std::sync::Mutex<Option<(CatalogKey, Option<ModelsFile>)>>;

static CATALOG_CACHE: std::sync::OnceLock<CatalogCache> = std::sync::OnceLock::new();
static CATALOG_GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Cheap identity for the config database: (mtime millis, len). `None` when the
/// file does not exist yet, which is itself a valid cache key.
fn catalog_stamp(home: &std::path::Path) -> Option<(u64, u64)> {
    let meta = std::fs::metadata(home.join("config.redb")).ok()?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)?;
    Some((mtime, meta.len()))
}

/// Drop the cached catalog. Every `models` write calls this; a no-op when the
/// cache is cold, so it is safe to call from the write path unconditionally.
pub fn invalidate_catalog() {
    CATALOG_GENERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

fn catalog_key(home: &std::path::Path) -> CatalogKey {
    CatalogKey {
        generation: CATALOG_GENERATION.load(std::sync::atomic::Ordering::Relaxed),
        stamp: catalog_stamp(home),
    }
}

/// Run `f` against the resolved catalog without cloning it. Hot paths (per
/// message, per event, per transcript row) must use this rather than `load()`,
/// which hands back a full copy of the bundled catalog.
pub fn with_catalog<T>(home: &std::path::Path, f: impl FnOnce(&ModelsFile) -> T) -> Option<T> {
    let key = catalog_key(home);
    let cache = CATALOG_CACHE.get_or_init(|| std::sync::Mutex::new(None));
    let mut guard = cache.lock().ok()?;
    if guard.as_ref().is_none_or(|(cached, _)| *cached != key) {
        // Rebuild under the lock so concurrent readers wait for one load
        // instead of each starting their own.
        *guard = Some((key, load_for(home)));
    }
    guard
        .as_ref()
        .and_then(|(_, catalog)| catalog.as_ref())
        .map(f)
}

/// Pricing for one model id, without cloning the catalog. This is the lookup
/// the RPC wire encoder needs for every assistant message.
pub fn cost_for(model: &str) -> Option<ModelCost> {
    with_catalog(&config::feather_home(), |cat| {
        find(cat, model).and_then(|(_, _, m)| m.cost.clone())
    })
    .flatten()
}

pub fn load() -> Option<ModelsFile> {
    load_cached(&config::feather_home())
}

/// Cached `load_for`. Prefer this over `load_for` for anything that can run
/// more than once per user action.
pub fn load_cached(home: &std::path::Path) -> Option<ModelsFile> {
    with_catalog(home, ModelsFile::clone)
}

pub fn load_for(home: &std::path::Path) -> Option<ModelsFile> {
    let mut value = crate::config_store::read(home, "models")
        .ok()
        .flatten()
        .unwrap_or_else(|| serde_json::json!({}));
    let deleted = value
        .get_mut("deletedModels")
        .map(serde_json::Value::take)
        .unwrap_or_default();
    let user = serde_json::from_value::<ModelsFile>(value.clone()).ok();
    let mut catalog = match (builtin_catalog(), user) {
        (Some(b), Some(u)) => merge_catalogs(b, u),
        (Some(b), None) => b.clone(),
        (None, Some(u)) => u,
        _ => return None,
    };
    if let Some(deleted) = deleted.as_array() {
        for item in deleted.iter().filter_map(|v| v.as_str()) {
            let Some((pid, id)) = item.split_once('/') else {
                continue;
            };
            // Built-in providers own their model list, so a stale deletion entry
            // for one (from an older build) is ignored instead of hiding the
            // bundled model.
            if is_managed_provider(pid) {
                continue;
            }
            if let Some(p) = catalog.providers.get_mut(pid) {
                p.models.retain(|m| m.id != id);
            }
        }
    }
    // Models discovered from a provider's own list endpoint. Unioned after
    // deletion filtering, so a stale `deletedModels` entry cannot hide a model
    // the provider actively serves, and never in place of a bundled entry: the
    // bundled list stays authoritative when discovery fails.
    for (pid, p) in catalog.providers.iter_mut() {
        for discovered in crate::model_refresh::cached_models(home, pid) {
            if p.models.iter().any(|m| m.id == discovered.id) {
                continue;
            }
            // The endpoint under-reports capability (it omits `image` even for
            // multimodal models), so a discovered variant adopts its bundled
            // sibling's input when the endpoint does not report more.
            let mut entry = discovered;
            let sibling =
                crate::model_refresh::family_of(&entry.id, p.models.iter().map(|m| m.id.as_str()))
                    .and_then(|id| p.models.iter().find(|m| m.id == id))
                    .cloned();
            if let Some(sibling) = sibling {
                let endpoint_has_image = entry.input.iter().any(|i| i == "image");
                let sibling_has_image = sibling.input.iter().any(|i| i == "image");
                if !endpoint_has_image && sibling_has_image {
                    entry.input = sibling.input;
                }
            }
            p.models.push(entry);
        }
    }
    apply_manual_effort(&mut catalog);
    Some(catalog)
}

/// Persisted models only, with routing defaults resolved. Do not fetch the
/// thousands of bundled models merely because a provider credential exists.
pub fn saved_for(home: &std::path::Path) -> std::io::Result<ModelsFile> {
    let value = crate::config_store::read(home, "models")?.unwrap_or_else(|| serde_json::json!({}));
    let all =
        load_cached(home).ok_or_else(|| std::io::Error::other("model catalog unavailable"))?;
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
pub fn save_for(home: &std::path::Path, catalog: &ModelsFile) -> std::io::Result<()> {
    let value = serde_json::to_value(catalog).map_err(std::io::Error::other)?;
    crate::config_store::update(home, "models", |v| {
        v["providers"] = value["providers"].clone();
    })
}

/// Validate the global context/compaction budget without overflowing arithmetic.
pub fn validate_context_limits(
    context_window: u64,
    reserve_tokens: u64,
    keep_recent_tokens: u64,
) -> std::io::Result<()> {
    if context_window == 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "context window must be positive",
        ));
    }
    if reserve_tokens >= context_window {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "reserve tokens must be below the context window",
        ));
    }
    if keep_recent_tokens >= context_window {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "preserved tokens must be below the input compaction budget",
        ));
    }
    Ok(())
}

/// The context window the agent actually runs with: the global context setting
/// capped by what the selected model accepts. A model without an advertised
/// window (custom endpoints) simply follows the global setting.
pub fn effective_context_window(global_context: u64, model: Option<&ModelEntry>) -> u64 {
    match model.and_then(|m| m.context_window) {
        Some(cap) if cap > 0 => global_context.min(cap),
        _ => global_context,
    }
}

/// Effective context window of `provider/id` (or a bare model id) in the
/// catalog, falling back to the global setting when the model is unknown.
pub fn effective_context_for(cat: &ModelsFile, global_context: u64, model: &str) -> u64 {
    effective_context_window(global_context, find(cat, model).map(|(_, _, m)| m))
}

pub fn save(catalog: &ModelsFile) -> std::io::Result<()> {
    let value = serde_json::to_value(catalog).map_err(std::io::Error::other)?;
    crate::config_store::update(&config::feather_home(), "models", |v| {
        v["providers"] = value["providers"].clone();
    })
}
pub fn upsert_model(pid: &str, provider: ProviderEntry, model: ModelEntry) -> std::io::Result<()> {
    guard_managed(pid)?;
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
    guard_managed(pid)?;
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
    fn catalog_cache_follows_writes_and_reflects_external_changes() {
        // The resolved catalog is memoized because rebuilding it per wire
        // message is far too expensive. A write must still be visible
        // immediately, and an outside writer must not be masked forever.
        let home = std::env::temp_dir().join(feather_core::provider::gen_id("catalog-cache"));
        crate::config_store::write(
            &home,
            "models",
            &serde_json::json!({"providers":{"custom":{"baseUrl":"https://one.test/v1","models":[{"id":"m"}]}}}),
        )
        .unwrap();
        let first = load_cached(&home).unwrap();
        assert!(first.providers.contains_key("custom"));
        assert_eq!(first.providers["custom"].models.len(), 1);

        // A store write invalidates the cache, so the new model is visible.
        crate::config_store::update(&home, "models", |value| {
            value["providers"]["custom"]["models"] = serde_json::json!([{"id": "m"}, {"id": "n"}]);
        })
        .unwrap();
        let second = load_cached(&home).unwrap();
        assert_eq!(second.providers["custom"].models.len(), 2);

        // Repeated reads reuse the cached catalog (same content, no rebuild).
        let third = load_cached(&home).unwrap();
        assert_eq!(third.providers["custom"].models.len(), 2);

        // A stamp change also invalidates the cache, so a write that bypasses
        // the store (another process, or a hand-edited document) is picked up.
        let mut external = crate::config_store::read(&home, "models").unwrap().unwrap();
        external["providers"]["custom"]["models"] = serde_json::json!([{"id": "m"}]);
        invalidate_catalog();
        crate::config_store::write(&home, "models", &external).unwrap();
        let fourth = load_cached(&home).unwrap();
        assert_eq!(fourth.providers["custom"].models.len(), 1);

        // Borrowed access runs the closure against the cached catalog without
        // cloning it, and reports "no such entry" as `None`.
        let ids = with_catalog(&home, |cat| {
            cat.providers["custom"]
                .models
                .iter()
                .map(|m| m.id.clone())
                .collect::<Vec<_>>()
        })
        .unwrap();
        assert_eq!(ids, vec!["m".to_string()]);
        assert!(with_catalog(&home, |cat| find(cat, "custom/absent").is_some()) == Some(false));
        std::fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn persisted_model_deletions_survive_reload_and_builtins_are_read_only() {
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
        // Built-in providers own their model list: delete/upsert are refused and
        // no `deletedModels` entry is written.
        assert!(delete_model_for(&home, pid, id).is_err());
        assert!(load_for(&home).unwrap().providers[pid]
            .models
            .iter()
            .any(|m| &m.id == id));
        crate::config_store::write(
            &home,
            "models",
            &serde_json::json!({"providers":{"custom":{"models":[{"id":"one"},{"id":"two"}]}}}),
        )
        .unwrap();
        delete_model_for(&home, "custom", "one").unwrap();
        for _ in 0..2 {
            let cat = load_for(&home).unwrap();
            assert_eq!(cat.providers["custom"].models.len(), 1);
            assert_eq!(cat.providers["custom"].models[0].id, "two");
        }
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn upsert_model_rejects_builtin_providers() {
        let builtin = builtin_catalog().unwrap();
        let pid = builtin
            .providers
            .iter()
            .find(|(_, p)| !p.models.is_empty())
            .map(|(id, _)| id.clone())
            .unwrap();
        let provider: ProviderEntry =
            serde_json::from_value(serde_json::json!({"baseUrl": "https://example.test/v1"}))
                .unwrap();
        let model: ModelEntry =
            serde_json::from_value(serde_json::json!({"id": "hijacked"})).unwrap();
        let err = upsert_model(&pid, provider, model).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
        assert!(is_managed_provider(&pid));
        assert!(!is_managed_provider("my-local-server"));
    }

    #[test]
    fn effective_context_is_the_smaller_of_global_and_model() {
        let model: ModelEntry =
            serde_json::from_value(serde_json::json!({"id": "m", "contextWindow": 128_000}))
                .unwrap();
        assert_eq!(effective_context_window(200_000, Some(&model)), 128_000);
        assert_eq!(effective_context_window(64_000, Some(&model)), 64_000);
        // No advertised window (custom endpoints) follows the global setting.
        let bare: ModelEntry = serde_json::from_value(serde_json::json!({"id": "m"})).unwrap();
        assert_eq!(effective_context_window(200_000, Some(&bare)), 200_000);
        assert_eq!(effective_context_window(200_000, None), 200_000);
    }

    #[test]
    fn volcano_models_resolve_to_the_ark_coding_endpoint() {
        // Regression: saving a model from the custom editor used to rewrite a
        // bundled provider's `api`, and a stale persisted entry could point the
        // Coding Plan at an unrelated gateway. Every Volcano model must resolve
        // to the Ark OpenAI-compatible endpoint with its metadata intact.
        let cat = builtin_catalog().unwrap();
        let (pid, p, m) = find(&cat, "volcengine-plan/deepseek-v4.1-flash").unwrap();
        assert_eq!(pid, "volcengine-plan");
        assert_eq!(p.api.as_deref(), Some("openai-completions"));
        assert_eq!(
            p.base_url.as_deref(),
            Some("https://ark.cn-beijing.volces.com/api/coding/v3")
        );
        assert!(m.reasoning && !m.reasoning_not_supported);
        // The duplicate `volcano` entry was folded into the Coding Plan provider.
        assert!(cat.providers.get("volcano").is_none());
        assert!(is_volcengine_provider("volcano"));
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

    #[test]
    fn builtin_provider_routing_cannot_be_overridden_by_persisted_entries() {
        // A saved/stale `baseUrl` or `api` for a bundled provider sent requests
        // to the wrong server (the Volcano Coding Plan pointed at an unrelated
        // gateway). Routing now always comes from the bundled catalog, while the
        // credential and user-only compat keys are kept.
        let builtin: ModelsFile = serde_json::from_str(
            r#"{"providers":{"volcengine-plan":{
                 "baseUrl":"https://ark.cn-beijing.volces.com/api/coding/v3",
                 "api":"openai-completions",
                 "compat":{"thinkingFormat":"deepseek"},
                 "models":[{"id":"glm-5.3-flash"}]}}}"#,
        )
        .unwrap();
        let user: ModelsFile = serde_json::from_str(
            r#"{"providers":{"volcengine-plan":{
                 "baseUrl":"https://token-plan.example.invalid/compatible-mode/v1",
                 "api":"anthropic-messages",
                 "apiKey":"ark-synthetic",
                 "compat":{"sendSessionAffinityHeaders":true},
                 "modelOverrides":{"glm-5.3-flash":{"compat":{"supportsLongCacheRetention":true}}},
                 "models":[]}}}"#,
        )
        .unwrap();
        let merged = merge_catalogs(&builtin, user);
        let plan = merged.providers.get("volcengine-plan").unwrap();
        assert_eq!(
            plan.base_url.as_deref(),
            Some("https://ark.cn-beijing.volces.com/api/coding/v3")
        );
        assert_eq!(plan.api.as_deref(), Some("openai-completions"));
        assert_eq!(plan.api_key.as_deref(), Some("ark-synthetic"));
        let compat = plan.compat.as_ref().unwrap();
        assert_eq!(compat["thinkingFormat"], serde_json::json!("deepseek"));
        assert_eq!(
            compat["sendSessionAffinityHeaders"],
            serde_json::json!(true)
        );
        // The bundled model list is restored for a built-in provider.
        assert_eq!(plan.models.len(), 1);
        assert_eq!(plan.models[0].id, "glm-5.3-flash");
    }
    #[test]
    fn volcengine_plan_levels_keep_glm_forced_thinking_and_allow_off_elsewhere() {
        // GLM-5.3 family on Ark rejects `thinking.type: "disabled"`, so `off`
        // must be reported as unsupported for it; other coding-plan models keep
        // the toggle. Both come from the bundled catalog plus the derived maps.
        let mut catalog = builtin_catalog().cloned().expect("builtin catalog");
        apply_manual_effort(&mut catalog);
        let plan = catalog.providers.get("volcengine-plan").unwrap();
        let glm = plan
            .models
            .iter()
            .find(|m| m.id == "glm-5.3-flash")
            .unwrap();
        assert_eq!(
            thinking_levels(glm, plan),
            vec!["low", "medium", "high", "xhigh", "max"]
        );
        let deepseek = plan
            .models
            .iter()
            .find(|m| m.id == "deepseek-v4-pro")
            .unwrap();
        assert_eq!(
            thinking_levels(deepseek, plan),
            vec!["off", "low", "medium", "high", "xhigh", "max"]
        );
        assert!(glm.reasoning && deepseek.reasoning);
    }

    #[test]
    fn volcengine_plan_keeps_an_explicit_model_map() {
        let mut catalog = builtin_catalog().cloned().expect("builtin catalog");
        let plan = catalog.providers.get_mut("volcengine-plan").unwrap();
        let pin = plan.models.iter_mut().find(|m| m.id == "kimi-k3").unwrap();
        pin.thinking_level_map = Some(serde_json::json!({"off": null, "high": "high"}));
        apply_manual_effort(&mut catalog);
        let plan = catalog.providers.get("volcengine-plan").unwrap();
        let pin = plan.models.iter().find(|m| m.id == "kimi-k3").unwrap();
        // An explicit map wins over the derived one: `off` stays unsupported, and
        // the other levels follow pi's "absent means supported" rule.
        assert_eq!(
            thinking_levels(pin, plan),
            vec!["minimal", "low", "medium", "high"]
        );
    }
}
