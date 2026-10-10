//! Pure, host-independent ports of `pi-cache-optimizer`'s decision logic.
//!
//! Only the parts of the plugin that are **pure functions over request or
//! response payloads** are ported here — the pieces whose TypeScript
//! behaviour is fully specified by `index.ts`/`src/retention.ts`/
//! `src/request-payload.ts` and does not depend on Pi's host API. The
//! host-coupled half of the plugin (event hooks, `models.json` repair,
//! footer stat shards, routing-registry symbols, environment mutation, tool
//! ordering) has no Rust counterpart by design; see the crate docs for the
//! precise boundary.
//!
//! Every function below is a direct transliteration, including the exact
//! field names and the wire order Anthropic uses (`tools → system →
//! messages`). Where the plugin mutates a caller-owned object we mirror that
//! with `&mut serde_json::Value`; where it returns a repaired copy we return
//! `Option<Value>` and never mutate the input.

use serde_json::{Map, Value};

/// `openai-completions` caps `prompt_cache_key` at 64 characters.
pub const OPENAI_PROMPT_CACHE_KEY_MAX_LENGTH: usize = 64;

// ---------------------------------------------------------------------------
// Retention (src/retention.ts)
// ---------------------------------------------------------------------------

/// The three retention states Pi threads through its providers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CacheRetention {
    Short,
    Long,
    None,
}

/// `resolveCacheRetention`: an explicit choice wins, otherwise
/// `PI_CACHE_RETENTION=long` maps to [`CacheRetention::Long`], else `Short`.
pub fn resolve_cache_retention(explicit: Option<CacheRetention>, env_long: bool) -> CacheRetention {
    explicit.unwrap_or(if env_long {
        CacheRetention::Long
    } else {
        CacheRetention::Short
    })
}

/// Why `prompt_cache_retention` was kept on, or removed from, a payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PromptCacheRetentionReason {
    ProviderRejected,
    OfficialOpenAi,
    ExplicitOptIn,
    UserRequestedLong,
    UnverifiedEndpoint,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PromptCacheRetentionGateInput {
    pub provider_rejected: bool,
    pub official_openai: bool,
    pub explicit_opt_in: bool,
    pub user_requested_long: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PromptCacheRetentionGateDecision {
    pub keep: bool,
    pub reason: PromptCacheRetentionReason,
}

/// The optimizer only undoes its own side effect: it never strips a field the
/// user or the provider config asked for, except after an explicit rejection.
pub fn decide_prompt_cache_retention(
    input: PromptCacheRetentionGateInput,
) -> PromptCacheRetentionGateDecision {
    if input.provider_rejected {
        return PromptCacheRetentionGateDecision {
            keep: false,
            reason: PromptCacheRetentionReason::ProviderRejected,
        };
    }
    if input.official_openai {
        return PromptCacheRetentionGateDecision {
            keep: true,
            reason: PromptCacheRetentionReason::OfficialOpenAi,
        };
    }
    if input.explicit_opt_in {
        return PromptCacheRetentionGateDecision {
            keep: true,
            reason: PromptCacheRetentionReason::ExplicitOptIn,
        };
    }
    if input.user_requested_long {
        return PromptCacheRetentionGateDecision {
            keep: true,
            reason: PromptCacheRetentionReason::UserRequestedLong,
        };
    }
    PromptCacheRetentionGateDecision {
        keep: false,
        reason: PromptCacheRetentionReason::UnverifiedEndpoint,
    }
}

/// Pi's Anthropic `getCacheControl`: `None` retention emits no `cache_control`;
/// long retention with `supportsLongCacheRetention` upgrades to the 1h TTL.
pub fn anthropic_cache_control(retention: CacheRetention, supports_long: bool) -> Option<Value> {
    if retention == CacheRetention::None {
        return None;
    }
    let mut control = Map::new();
    control.insert("type".into(), Value::String("ephemeral".into()));
    if retention == CacheRetention::Long && supports_long {
        control.insert("ttl".into(), Value::String("1h".into()));
    }
    Some(Value::Object(control))
}

// ---------------------------------------------------------------------------
// Anthropic TTL ordering (src/request-payload.ts)
// ---------------------------------------------------------------------------

/// One `cache_control` marker observed in wire order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnthropicCacheControl {
    pub ttl: Option<String>,
}

/// Collect `type === "ephemeral"` cache controls in the order Anthropic
/// processes them: `tools → system → messages`.
pub fn collect_anthropic_cache_controls_in_wire_order(
    payload: &Value,
) -> Vec<AnthropicCacheControl> {
    let mut out = Vec::new();
    let Some(record) = payload.as_object() else {
        return out;
    };
    let collect = |value: &Value, out: &mut Vec<AnthropicCacheControl>| {
        let Some(cc) = value
            .as_object()
            .and_then(|o| o.get("cache_control"))
            .and_then(|c| c.as_object())
        else {
            return;
        };
        if cc.get("type").and_then(|t| t.as_str()) != Some("ephemeral") {
            return;
        }
        out.push(AnthropicCacheControl {
            ttl: cc.get("ttl").and_then(|t| t.as_str()).map(str::to_string),
        });
    };
    if let Some(tools) = record.get("tools").and_then(|t| t.as_array()) {
        for tool in tools {
            collect(tool, &mut out);
        }
    }
    if let Some(system) = record.get("system").and_then(|s| s.as_array()) {
        for block in system {
            collect(block, &mut out);
        }
    }
    if let Some(messages) = record.get("messages").and_then(|m| m.as_array()) {
        for message in messages {
            if let Some(content) = message
                .as_object()
                .and_then(|o| o.get("content"))
                .and_then(|c| c.as_array())
            {
                for block in content {
                    collect(block, &mut out);
                }
            }
        }
    }
    out
}

/// Does the payload put a 1h breakpoint after a 5m/default one?
pub fn has_anthropic_cache_ttl_order_conflict(payload: &Value) -> bool {
    let mut seen_short = false;
    for control in collect_anthropic_cache_controls_in_wire_order(payload) {
        match control.ttl.as_deref() {
            None | Some("5m") => seen_short = true,
            Some("1h") if seen_short => return true,
            _ => {}
        }
    }
    false
}

/// Does the payload carry any 1h breakpoint?
pub fn has_anthropic_long_cache_control(payload: &Value) -> bool {
    collect_anthropic_cache_controls_in_wire_order(payload)
        .iter()
        .any(|c| c.ttl.as_deref() == Some("1h"))
}

/// In-place downgrade of every 1h breakpoint to the default 5m retention.
/// Returns whether anything changed.
pub fn downgrade_anthropic_long_cache_controls(payload: &mut Value) -> bool {
    let mut changed = false;
    let mut downgrade = |value: &mut Value| {
        if let Some(cc) = value
            .as_object_mut()
            .and_then(|o| o.get_mut("cache_control"))
            .and_then(|c| c.as_object_mut())
        {
            if cc.get("ttl").and_then(|t| t.as_str()) == Some("1h") {
                cc.remove("ttl");
                changed = true;
            }
        }
    };
    if let Some(record) = payload.as_object_mut() {
        if let Some(tools) = record.get_mut("tools").and_then(|t| t.as_array_mut()) {
            for tool in tools {
                downgrade(tool);
            }
        }
        if let Some(system) = record.get_mut("system").and_then(|s| s.as_array_mut()) {
            for block in system {
                downgrade(block);
            }
        }
        if let Some(messages) = record.get_mut("messages").and_then(|m| m.as_array_mut()) {
            for message in messages {
                if let Some(content) = message
                    .as_object_mut()
                    .and_then(|o| o.get_mut("content"))
                    .and_then(|c| c.as_array_mut())
                {
                    for block in content {
                        downgrade(block);
                    }
                }
            }
        }
    }
    changed
}

/// Returns a repaired copy when the payload needs a downgrade, or `None` when
/// it is already valid. `force_downgrade` is the process-local fallback for
/// models that already returned the TTL-order error (proxies can inject hidden
/// short breakpoints after the request hook).
pub fn with_anthropic_cache_ttl_repair(payload: &Value, force_downgrade: bool) -> Option<Value> {
    let needs_repair = has_anthropic_cache_ttl_order_conflict(payload)
        || (force_downgrade && has_anthropic_long_cache_control(payload));
    if !needs_repair {
        return None;
    }
    let mut copy = payload.clone();
    downgrade_anthropic_long_cache_controls(&mut copy);
    Some(copy)
}

/// Recognise Anthropic's explicit TTL-ordering error on a finalized assistant
/// message (`role == "assistant"`, `stopReason == "error"`).
pub fn has_anthropic_cache_ttl_order_error(message: &Value) -> bool {
    let Some(record) = message.as_object() else {
        return false;
    };
    if record.get("role").and_then(|r| r.as_str()) != Some("assistant") {
        return false;
    }
    if record.get("stopReason").and_then(|r| r.as_str()) != Some("error") {
        return false;
    }
    let Some(error) = record.get("errorMessage").and_then(|e| e.as_str()) else {
        return false;
    };
    let error = error.to_lowercase();
    error.contains("cache_control")
        && error.contains("ttl='1h'")
        && error.contains("ttl='5m'")
        && error.contains("must not come after")
}

// ---------------------------------------------------------------------------
// OpenAI-compatible payload safety (src/request-payload.ts)
// ---------------------------------------------------------------------------

/// `clampPromptCacheKey`: trim, reject empty, truncate to 64 characters.
pub fn clamp_prompt_cache_key(key: Option<&str>) -> Option<String> {
    let normalized = key?.trim();
    if normalized.is_empty() {
        return None;
    }
    Some(
        normalized
            .chars()
            .take(OPENAI_PROMPT_CACHE_KEY_MAX_LENGTH)
            .collect(),
    )
}

pub fn has_effective_prompt_cache_key(record: &Map<String, Value>) -> bool {
    ["prompt_cache_key", "promptCacheKey"].iter().any(|k| {
        record
            .get(*k)
            .and_then(|v| v.as_str())
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false)
    })
}

/// Add `prompt_cache_key` only when the payload does not already carry an
/// effective key. Returns a copy, or `None` when nothing should change.
pub fn add_openai_prompt_cache_key(payload: &Value, cache_key: Option<&str>) -> Option<Value> {
    let record = payload.as_object()?;
    let key = clamp_prompt_cache_key(cache_key)?;
    if has_effective_prompt_cache_key(record) {
        return None;
    }
    let mut copy = record.clone();
    copy.insert("prompt_cache_key".into(), Value::String(key));
    Some(Value::Object(copy))
}

/// Remove both spellings of the key; `None` when neither was present.
pub fn omit_openai_prompt_cache_keys(payload: &Value) -> Option<Value> {
    let record = payload.as_object()?;
    if !record.contains_key("prompt_cache_key") && !record.contains_key("promptCacheKey") {
        return None;
    }
    let mut copy = record.clone();
    copy.remove("prompt_cache_key");
    copy.remove("promptCacheKey");
    Some(Value::Object(copy))
}

/// Remove `prompt_cache_retention`; `None` when it was absent.
pub fn without_prompt_cache_retention(payload: &Value) -> Option<Value> {
    let record = payload.as_object()?;
    if !record.contains_key("prompt_cache_retention") {
        return None;
    }
    let mut copy = record.clone();
    copy.remove("prompt_cache_retention");
    Some(Value::Object(copy))
}

// ---------------------------------------------------------------------------
// Session identity (src/request-payload.ts)
// ---------------------------------------------------------------------------

/// `derivePromptCacheKey`: a one-way `pi-<sha256(session_id)[..32 hex]>` so the
/// raw session id never reaches an endpoint. Empty/blank ids map to `None`.
pub fn derive_prompt_cache_key(session_id: Option<&str>) -> Option<String> {
    let normalized = session_id?.trim();
    if normalized.is_empty() {
        return None;
    }
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(normalized.as_bytes());
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    Some(format!("pi-{}", &hex[..32]))
}
