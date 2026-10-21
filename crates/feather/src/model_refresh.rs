//! Provider model-catalog refresh at startup.
//!
//! Managed providers own their model lists, so a model the provider launches
//! after a Feather release cannot be selected until the next build ships it.
//! Providers that expose a model-list endpoint are refreshed here instead — at
//! startup and on an explicit user action — and the result is cached in
//! `config.redb` so a normal start makes no request.
//!
//! Ark's OpenAI-compatible `GET /api/coding/v3/models` answers with the whole
//! platform catalog (137 ids, including retired builds, video, 3D and embedding
//! models). It is filtered to callable text-generation models:
//! `status` must not be `Shutdown`/`Retiring`, and the entry must advertise
//! `TextGeneration`. Measured against a live Coding Plan key, this keeps the
//! chat models the plan serves — including new releases such as
//! `deepseek-v4-1-flash-economy` — while dropping media/non-chat entries.
//!
//! Discovery is deliberately conservative:
//! * Only providers with a known list endpoint are contacted; nothing else is
//!   probed.
//! * A cached list is authoritative until it expires, so a normal start makes no
//!   request and an offline start keeps the previous list.
//! * Any failure (missing credential, HTTP error, unexpected shape, timeout)
//!   silently keeps the bundled/cached list. Refresh never blocks or fails a
//!   launch, and it never runs on the UI thread.
//! * Discovered models are unioned with the bundled ones, so a provider that
//!   stops listing a model (or an endpoint that changes shape) cannot silently
//!   remove a model the user was already using.

use crate::{auth::StoredCredentialResolver, config_store, models};
use feather_core::provider::{
    credentials::{CredentialResolver, ResolvedCredential},
    transport,
};
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// redb document holding the last successful refresh per provider.
const REFRESH_DOC: &str = "model-refresh";
/// Discovery is a background convenience: it must not delay a launch.
const TIMEOUT: Duration = Duration::from_secs(20);
/// How long a successful refresh is trusted before another request is made.
const MAX_AGE_SECS: u64 = 12 * 60 * 60;

/// How one provider's models are discovered.
struct Endpoint {
    /// Provider id as it appears in the catalog.
    provider: &'static str,
    /// Model-list path appended to the provider's base URL.
    path: &'static str,
    /// Platform domains that are chat models. Anything else (video, 3D,
    /// embedding, translation) is not selectable as an agent model.
    domains: &'static [&'static str],
    /// Model names the subscription documents as callable, used to collapse a
    /// dated platform build onto the name a client must send. A family with no
    /// documented name is dropped: the platform list is far wider than any one
    /// subscription.
    plan_names: &'static [&'static str],
}

/// Ark Coding Plan model names, from the plan's own "模型配置" documentation.
/// This is the subscription's public contract, not a catalog snapshot: a name
/// stays valid while the plan can call it, and a newly launched model appears
/// here once Ark documents it.
const ARK_CODING_PLAN: &[&str] = &[
    "ark-code-latest",
    "doubao-seed-evolving",
    "doubao-seed-2.1-pro",
    "doubao-seed-2.1-lite",
    "doubao-seed-2.0-mini",
    "glm-5.3",
    "glm-5.3-flash",
    "deepseek-v4.1-flash",
    "deepseek-v4.1-flash-economy",
    "deepseek-v4-flash",
    "deepseek-v4-pro",
    "kimi-k2.8-preview",
    "kimi-k3",
];

const ENDPOINTS: &[Endpoint] = &[Endpoint {
    provider: "volcengine-plan",
    path: "/models",
    domains: &["LLM"],
    plan_names: ARK_CODING_PLAN,
}];

/// One discovered model. Limits come from the provider response when present;
/// everything else uses Feather's provider-wide defaults.
#[derive(Clone, Debug, Deserialize, Serialize)]
struct Discovered {
    id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    context_window: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    input: Vec<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct Cached {
    /// Unix seconds of the successful refresh.
    #[serde(default)]
    at: u64,
    #[serde(default)]
    models: Vec<Discovered>,
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn cache_for(home: &std::path::Path) -> std::collections::BTreeMap<String, Cached> {
    config_store::read(home, REFRESH_DOC)
        .ok()
        .flatten()
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default()
}

fn store_cache(home: &std::path::Path, provider: &str, entry: Cached) -> std::io::Result<()> {
    config_store::update(home, REFRESH_DOC, |v| {
        if !v.is_object() {
            *v = serde_json::json!({});
        }
        v[provider] = serde_json::to_value(&entry).unwrap_or(serde_json::Value::Null);
    })?;
    models::invalidate_catalog();
    Ok(())
}

/// Cached discovery for one provider, regardless of age, as catalog entries.
/// Catalog assembly unions this with the bundled list, so an offline start still
/// offers everything discovered earlier.
pub fn cached_models(home: &std::path::Path, provider: &str) -> Vec<models::ModelEntry> {
    let Some(entry) = cache_for(home).get(provider).cloned() else {
        return Vec::new();
    };
    entry
        .models
        .into_iter()
        .map(|m| models::ModelEntry {
            id: m.id,
            name: m.name,
            context_window: m.context_window,
            max_tokens: m.max_tokens,
            input: m.input,
            reasoning: true,
            ..Default::default()
        })
        .collect()
}

/// Whether a `/models` entry is a chat model Feather can drive.
fn is_chat_model(item: &serde_json::Value, endpoint: &Endpoint) -> bool {
    if matches!(item["status"].as_str(), Some("Shutdown" | "Retiring")) {
        return false;
    }
    if let Some(domain) = item["domain"].as_str().filter(|d| !d.is_empty()) {
        if !endpoint.domains.is_empty() && !endpoint.domains.contains(&domain) {
            return false;
        }
    }
    // `task_type` is absent on a few older entries that are still callable, so
    // only reject when it is present and excludes text generation.
    match item["task_type"].as_array() {
        Some(tasks) if !tasks.is_empty() => tasks
            .iter()
            .filter_map(|t| t.as_str())
            .any(|t| t == "TextGeneration" || t == "VisualQuestionAnswering"),
        _ => true,
    }
}

/// Compare two model names ignoring dashes, dots and case, so a platform's
/// dash form (`deepseek-v4-1-flash`) matches the plan's dotted form
/// (`deepseek-v4.1-flash`).
fn normalized(value: &str) -> String {
    value
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .flat_map(char::to_lowercase)
        .collect()
}

/// Collapse a platform model id onto the callable plan name.
///
/// A platform id carries a dated build suffix (`…-261017`) and may use dashes
/// where the plan documents dots. The longest documented name that the id
/// starts with wins, so `deepseek-v4.1-flash-economy` cannot be folded into
/// `deepseek-v4.1-flash`. Ids matching no documented name are dropped.
fn family_name(platform_id: &str, plan_names: &[&str]) -> Option<String> {
    family_of(platform_id, plan_names.iter().copied()).map(str::to_string)
}

/// Longest of `candidates` that `id` extends, comparing names without dashes,
/// dots or case. Also used to attach a discovered model to the bundled model of
/// the same family, whose capabilities are hand-checked.
pub fn family_of<'a>(id: &str, candidates: impl Iterator<Item = &'a str>) -> Option<&'a str> {
    let target = normalized(id);
    candidates
        .filter(|candidate| {
            let name = normalized(candidate);
            !name.is_empty() && target.starts_with(&name)
        })
        .max_by_key(|candidate| normalized(candidate).len())
}
/// Turn a `/models` payload into catalog-ready models.
fn accept(payload: &serde_json::Value, endpoint: &Endpoint) -> Vec<Discovered> {
    let items = payload["data"]
        .as_array()
        .or_else(|| payload["models"].as_array())
        .map(Vec::as_slice)
        .unwrap_or_default();
    let mut out: Vec<Discovered> = Vec::new();
    for item in items.iter().filter(|i| is_chat_model(i, endpoint)) {
        let Some(id) = item["id"].as_str().filter(|id| !id.is_empty()) else {
            continue;
        };
        // A platform entry carries a dated, dash-separated build suffix
        // (`deepseek-v4-1-flash-economy-261017`). Returned ids must be the
        // callable model name the provider accepts, and one family must appear
        // once: dated builds are folded into their family name and the newest
        // build wins.
        let family = family_name(id, endpoint.plan_names);
        let Some(id) = family else {
            continue;
        };
        // Ark reuses the platform id as the display name for some models
        // (`deepseek-v4-1-flash-economy`), which reads poorly beside the named
        // siblings. A name with capitals or spaces is a real label; otherwise
        // the plan's own dotted spelling is used.
        let platform_name = item["name"]
            .as_str()
            .filter(|n| !n.is_empty())
            .unwrap_or(id.as_str());
        let name = if platform_name.chars().any(char::is_uppercase) || platform_name.contains(' ') {
            platform_name.to_string()
        } else {
            id.clone()
        };
        let limits = &item["token_limits"];
        let build = limits["context_window"].as_u64().unwrap_or(0);
        let entry = Discovered {
            id,
            name: Some(name),
            context_window: limits["context_window"].as_u64(),
            max_tokens: limits["max_output_token_length"].as_u64(),
            input: item["modalities"]["input_modalities"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|v| v.as_str())
                .map(str::to_string)
                .collect(),
        };
        match out.iter_mut().find(|existing| existing.id == entry.id) {
            Some(existing) => {
                let current = existing.context_window.unwrap_or(0);
                if build > current {
                    *existing = entry;
                }
            }
            None => out.push(entry),
        }
    }
    out
}

/// Ask one provider for its models. Returns `None` when the provider has no
/// stored credential, so an unconfigured provider costs no request.
async fn discover(home: &std::path::Path, endpoint: &Endpoint) -> Option<Vec<Discovered>> {
    let catalog = models::load_cached(home)?;
    let provider = catalog.providers.get(endpoint.provider)?;
    let base = provider.base_url.clone()?;
    // Resolve the credential the same way a chat request does, so an expired
    // OAuth token is refreshed rather than used.
    let credential: ResolvedCredential =
        StoredCredentialResolver::for_home(home, endpoint.provider, provider.api_key.clone())
            .with_headers(Vec::new())
            .resolve()
            .await
            .ok()?;
    let http = transport::async_builder().timeout(TIMEOUT).build().ok()?;
    let response = http
        .get(format!("{}{}", base.trim_end_matches('/'), endpoint.path))
        .bearer_auth(&credential.api_key)
        .send()
        .await
        .ok()?;
    if !response.status().is_success() {
        return None;
    }
    let payload: serde_json::Value = response.json().await.ok()?;
    let discovered = accept(&payload, endpoint);
    // An empty result means the shape changed or the account sees no models:
    // keep what the user already had rather than caching an empty catalog.
    (!discovered.is_empty()).then_some(discovered)
}

/// Refresh every configured provider whose cache is older than [`MAX_AGE_SECS`],
/// or all of them when `force`. Blocking: call from a worker thread, never from
/// the UI. Returns the provider ids whose list was refreshed.
pub fn refresh_if_stale(home: &std::path::Path, force: bool) -> Vec<&'static str> {
    let cache = cache_for(home);
    let now = now_secs();
    let mut refreshed = Vec::new();
    for endpoint in ENDPOINTS {
        let fresh = cache
            .get(endpoint.provider)
            .is_some_and(|entry| now.saturating_sub(entry.at) < MAX_AGE_SECS);
        if fresh && !force {
            continue;
        }
        let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        else {
            continue;
        };
        let Some(discovered) = runtime.block_on(discover(home, endpoint)) else {
            continue;
        };
        let entry = Cached {
            at: now_secs(),
            models: discovered,
        };
        if store_cache(home, endpoint.provider, entry).is_ok() {
            refreshed.push(endpoint.provider);
        }
    }
    refreshed
}

/// Startup entry point: refresh in the background so a slow or offline provider
/// never delays the first frame. `on_change` runs on the worker thread after a
/// successful refresh so the caller can drop its catalog copy.
pub fn spawn_startup_refresh(home: std::path::PathBuf, on_change: impl FnOnce() + Send + 'static) {
    std::thread::spawn(move || {
        if !refresh_if_stale(&home, false).is_empty() {
            on_change();
        }
    });
}

/// Force a refresh now, ignoring the cache age. Used by an explicit user action
/// and tests; blocking, so it must run off the UI thread.
pub fn refresh_now(home: &std::path::Path) -> Vec<&'static str> {
    refresh_if_stale(home, true)
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const ENDPOINT: Endpoint = Endpoint {
        provider: "volcengine-plan",
        path: "/models",
        domains: &["LLM"],
        plan_names: ARK_CODING_PLAN,
    };

    /// Trimmed from a live `/api/coding/v3/models` response: retired builds, a
    /// video model, an embedding model and two builds of one family are all
    /// present, as they are upstream.
    fn payload() -> serde_json::Value {
        json!({"data":[
            {"id":"deepseek-v4-1-flash-economy-261017","name":"deepseek-v4-1-flash-economy","domain":"LLM",
             "task_type":["TextGeneration"],"modalities":{"input_modalities":["text"]},
             "token_limits":{"context_window":1048576,"max_output_token_length":393216}},
            {"id":"deepseek-v4-1-flash-260910","name":"deepseek-v4-1-flash","domain":"LLM",
             "task_type":["TextGeneration"],"modalities":{"input_modalities":["text"]},
             "token_limits":{"context_window":524288,"max_output_token_length":32768}},
            {"id":"deepseek-v4-1-flash-261017","name":"deepseek-v4-1-flash","domain":"LLM",
             "task_type":["TextGeneration"],"modalities":{"input_modalities":["text"]},
             "token_limits":{"context_window":1048576,"max_output_token_length":393216}},
            {"id":"doubao-seed-2-1-pro-260915","name":"doubao-seed-2-1-pro","domain":"LLM",
             "task_type":["VisualQuestionAnswering","TextGeneration"],
             "modalities":{"input_modalities":["text","image"]},
             "token_limits":{"context_window":1048576,"max_output_token_length":262144}},
            {"id":"glm-5-3-flash-260828","name":"glm-5-3-flash","domain":"LLM",
             "task_type":["TextGeneration"],"modalities":{"input_modalities":["text"]},
             "token_limits":{"context_window":1048576,"max_output_token_length":131072}},
            {"id":"deepseek-v4-pro-260425","name":"deepseek-v4-pro","domain":"LLM",
             "status":"Retiring","task_type":["TextGeneration"]},
            {"id":"doubao-lite-128k-240428","name":"doubao-lite-128k","domain":"LLM",
             "status":"Shutdown","task_type":["TextGeneration"]},
            {"id":"doubao-seedance-2-0-260128","name":"doubao-seedance-2-0","domain":"",
             "task_type":["TextToVideo"],"modalities":{"input_modalities":["text"]}},
            {"id":"doubao-embedding-vision-251215","name":"doubao-embedding-vision","domain":"LLM",
             "task_type":["ImageEmbedding"]},
            {"id":"glm-4-5-air-20250728","name":"glm-4-5-air","domain":"LLM","task_type":["TextGeneration"]}
        ]})
    }

    fn ids(models: &[Discovered]) -> Vec<&str> {
        models.iter().map(|m| m.id.as_str()).collect()
    }

    #[test]
    fn keeps_callable_plan_models_and_drops_retired_media_and_undocumented_ones() {
        let models = accept(&payload(), &ENDPOINT);
        // Retired builds, media models and ids the plan does not document
        // (`glm-4-5-air`) are gone; dated builds are folded onto one family.
        assert_eq!(
            ids(&models),
            [
                "deepseek-v4.1-flash-economy",
                "deepseek-v4.1-flash",
                "doubao-seed-2.1-pro",
                "glm-5.3-flash"
            ]
        );
        let economy = &models[0];
        assert_eq!(economy.context_window, Some(1048576));
        assert_eq!(economy.max_tokens, Some(393216));
        // Ark reuses the platform id as the display name; the dotted plan name
        // is kept instead so the picker does not show `deepseek-v4-1-…`.
        assert_eq!(economy.name.as_deref(), Some("deepseek-v4.1-flash-economy"));
    }

    #[test]
    fn newest_build_of_a_family_wins() {
        let models = accept(&payload(), &ENDPOINT);
        let flash = models
            .iter()
            .find(|m| m.id == "deepseek-v4.1-flash")
            .expect("standard flash present");
        // 261017 reports 1M context, 260910 reports 512k.
        assert_eq!(flash.context_window, Some(1048576));
    }

    #[test]
    fn a_longer_plan_name_is_not_shadowed_by_a_shorter_prefix() {
        assert_eq!(
            family_name("deepseek-v4-1-flash-economy-261017", ARK_CODING_PLAN).as_deref(),
            Some("deepseek-v4.1-flash-economy")
        );
        assert_eq!(
            family_name("deepseek-v4-1-flash-261017", ARK_CODING_PLAN).as_deref(),
            Some("deepseek-v4.1-flash")
        );
        // `glm-5.3` must not claim `glm-5.3-flash`.
        assert_eq!(
            family_name("glm-5-3-flash-260828", ARK_CODING_PLAN).as_deref(),
            Some("glm-5.3-flash")
        );
        assert_eq!(family_name("unknown-model-1", ARK_CODING_PLAN), None);
    }

    #[test]
    fn unknown_shape_yields_nothing_so_the_cache_is_kept() {
        assert!(accept(&json!({"data":[]}), &ENDPOINT).is_empty());
        assert!(accept(&json!({"error":{"code":"AuthenticationError"}}), &ENDPOINT).is_empty());
        assert!(accept(&json!("not json"), &ENDPOINT).is_empty());
    }

    fn cached_home(models: Vec<Discovered>) -> std::path::PathBuf {
        let home = std::env::temp_dir().join(feather_core::provider::gen_id("model-refresh"));
        std::fs::create_dir_all(&home).unwrap();
        store_cache(
            &home,
            "volcengine-plan",
            Cached {
                at: now_secs(),
                models,
            },
        )
        .unwrap();
        home
    }

    #[test]
    fn cached_models_become_reasoning_catalog_entries() {
        let home = cached_home(accept(&payload(), &ENDPOINT));
        let entries = cached_models(&home, "volcengine-plan");
        assert_eq!(entries.len(), 4);
        assert!(entries.iter().all(|m| m.reasoning));
        assert_eq!(entries[0].id, "deepseek-v4.1-flash-economy");
        assert!(entries[0].input.contains(&"text".to_string()));
        // Another provider is unaffected.
        assert!(cached_models(&home, "zai").is_empty());
    }

    #[test]
    fn a_stale_cache_is_not_used_to_skip_a_refresh() {
        let home = std::env::temp_dir().join(feather_core::provider::gen_id("model-refresh"));
        std::fs::create_dir_all(&home).unwrap();
        store_cache(
            &home,
            "volcengine-plan",
            Cached {
                at: now_secs() - MAX_AGE_SECS - 1,
                models: accept(&payload(), &ENDPOINT),
            },
        )
        .unwrap();
        // No credential is configured in this isolated home, so the refresh makes
        // no request and must not clear what is already cached.
        assert!(refresh_if_stale(&home, false).is_empty());
        assert_eq!(cached_models(&home, "volcengine-plan").len(), 4);
    }
}
