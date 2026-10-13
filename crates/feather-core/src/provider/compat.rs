//! Provider compatibility metadata (`compat`) shared by the Anthropic and
//! OpenAI-compatible providers.
//!
//! feather has no dynamic provider registration. The catalog's `compat` object
//! (and the static model-ID metadata in `anthropic.rs`) is handed to a provider
//! through its builder. These helpers mirror only the small parts of pi-ai's
//! compatibility handling that feather actually implements; they deliberately do
//! not pretend to cover pi-ai's full compat surface.

use serde_json::Value;
use std::collections::BTreeMap;

/// pi's thinking levels, in order. Order matters for level clamping.
pub const THINKING_LEVELS: [&str; 7] = ["off", "minimal", "low", "medium", "high", "xhigh", "max"];

/// Shallow-merge two optional `compat` JSON objects; `over` wins per top-level
/// key. A missing side contributes nothing, and two missing sides yield `Null`.
pub fn merge_compat(base: Option<&Value>, over: Option<&Value>) -> Value {
    match (base, over) {
        (Some(Value::Object(a)), Some(Value::Object(b))) => {
            let mut merged = a.clone();
            for (k, v) in b {
                merged.insert(k.clone(), v.clone());
            }
            Value::Object(merged)
        }
        (_, Some(v)) => v.clone(),
        (Some(v), None) => v.clone(),
        (None, None) => Value::Null,
    }
}

/// pi models carry a top-level `compat` object next to top-level fields such as
/// `thinkingLevelMap`. The builder may be handed either a bare compat object or
/// that whole model-entry shape; resolve keys from both.
fn nested_compat(value: &Value) -> Option<&Value> {
    value.get("compat").filter(|v| v.is_object())
}

pub fn get_bool(compat: &Value, key: &str) -> Option<bool> {
    compat.get(key).and_then(Value::as_bool).or_else(|| {
        nested_compat(compat)
            .and_then(|c| c.get(key))
            .and_then(Value::as_bool)
    })
}

pub fn get_str<'a>(compat: &'a Value, key: &str) -> Option<&'a str> {
    compat.get(key).and_then(Value::as_str).or_else(|| {
        nested_compat(compat)
            .and_then(|c| c.get(key))
            .and_then(Value::as_str)
    })
}

pub fn get_u32(compat: &Value, key: &str) -> Option<u32> {
    let as_u32 = |v: &Value| v.as_u64().and_then(|v| u32::try_from(v).ok());
    compat.get(key).and_then(as_u32).or_else(|| {
        nested_compat(compat)
            .and_then(|c| c.get(key))
            .and_then(as_u32)
    })
}

/// Parse a `thinkingLevelMap` object (`level -> string | null`). The key may sit
/// at the top level (pi's model metadata) or inside a nested `compat` object.
/// Returns `None` when absent or not an object, so callers can fall back to the
/// static model metadata.
pub fn parse_level_map(compat: &Value) -> Option<BTreeMap<String, Value>> {
    let obj = compat
        .get("thinkingLevelMap")
        .and_then(Value::as_object)
        .or_else(|| {
            nested_compat(compat)
                .and_then(|c| c.get("thinkingLevelMap"))
                .and_then(Value::as_object)
        })?;
    Some(obj.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
}

/// pi's `getSupportedThinkingLevels` for a reasoning model. A level mapped to
/// `null` is unsupported; `xhigh`/`max` require an explicit mapping; every other
/// level is supported unless mapped to `null`.
pub fn supported_levels(map: &BTreeMap<String, Value>) -> Vec<&'static str> {
    THINKING_LEVELS
        .iter()
        .copied()
        .filter(|level| match map.get(*level) {
            Some(Value::Null) => false,
            None => !matches!(*level, "xhigh" | "max"),
            Some(_) => true,
        })
        .collect()
}

/// pi's `clampThinkingLevel`: if the requested level is unsupported, scan
/// forward then backward through the ordered level list for the nearest
/// supported one.
pub fn clamp_level(map: &BTreeMap<String, Value>, level: &str) -> String {
    let available = supported_levels(map);
    let fallback = || {
        available
            .first()
            .map(|s| s.to_string())
            .unwrap_or_else(|| "off".to_string())
    };
    if available.contains(&level) {
        return level.to_string();
    }
    let Some(index) = THINKING_LEVELS.iter().position(|l| *l == level) else {
        return fallback();
    };
    for candidate in &THINKING_LEVELS[index..] {
        if available.contains(candidate) {
            return candidate.to_string();
        }
    }
    for i in (0..index).rev() {
        if available.contains(&THINKING_LEVELS[i]) {
            return THINKING_LEVELS[i].to_string();
        }
    }
    fallback()
}

/// The `thinkingLevelMap` value for a level when it is a string.
pub fn mapped_effort(map: &BTreeMap<String, Value>, level: &str) -> Option<String> {
    match map.get(level) {
        Some(Value::String(s)) => Some(s.clone()),
        _ => None,
    }
}

/// `thinkingLevelMap[level] ?? level`: an explicit string wins, anything else
/// (missing or `null`) falls back to the raw level name.
pub fn effective_effort(map: &BTreeMap<String, Value>, level: &str) -> String {
    mapped_effort(map, level).unwrap_or_else(|| level.to_string())
}

/// Summaries use a cheap enabled level rather than the live turn's effort.
/// `None` would become `off` on some gateways, which reasoning-only models
/// reject. Respect an explicit non-reasoning model and clamp against its map.
pub(crate) fn compaction_thinking(
    compat: &Value,
    active: Option<super::ThinkingLevel>,
) -> Option<super::ThinkingLevel> {
    use super::ThinkingLevel;

    let reasoning = get_bool(compat, "reasoning")
        .unwrap_or(active.is_some_and(|level| level != ThinkingLevel::Off));
    if !reasoning {
        return None;
    }
    let level = parse_level_map(compat)
        .map(|map| clamp_level(&map, "low"))
        .unwrap_or_else(|| "low".to_string());
    ThinkingLevel::parse(&level).filter(|level| *level != ThinkingLevel::Off)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn map(v: Value) -> BTreeMap<String, Value> {
        parse_level_map(&json!({ "thinkingLevelMap": v })).unwrap()
    }

    #[test]
    fn supported_levels_follow_pi_rules() {
        // empty map: xhigh/max need an explicit mapping, everything else is on
        assert_eq!(
            supported_levels(&map(json!({}))),
            vec!["off", "minimal", "low", "medium", "high"]
        );
        // null removes a level; xhigh/max present
        let m = map(json!({ "off": null, "minimal": null, "xhigh": "xhigh", "max": "max" }));
        assert_eq!(
            supported_levels(&m),
            vec!["low", "medium", "high", "xhigh", "max"]
        );
    }

    #[test]
    fn clamp_scans_forward_then_backward() {
        // Claude 4.6 map: only max is explicit, so xhigh clamps up to max
        let m = map(json!({ "max": "max" }));
        assert_eq!(clamp_level(&m, "xhigh"), "max");
        assert_eq!(clamp_level(&m, "max"), "max");
        assert_eq!(clamp_level(&m, "high"), "high");
        // deepseek map: medium is null, so it clamps forward to high
        let d = map(
            json!({ "minimal": null, "low": "low", "medium": null, "high": "high", "max": "max" }),
        );
        assert_eq!(clamp_level(&d, "medium"), "high");
        assert_eq!(clamp_level(&d, "minimal"), "low");
        // no map: xhigh/max clamp backward to high
        assert_eq!(clamp_level(&map(json!({})), "xhigh"), "high");
        assert_eq!(clamp_level(&map(json!({})), "max"), "high");
    }

    #[test]
    fn effective_effort_prefers_strings_and_falls_back() {
        let m = map(json!({ "low": "low", "medium": null }));
        assert_eq!(effective_effort(&m, "low"), "low");
        assert_eq!(effective_effort(&m, "medium"), "medium");
        assert_eq!(mapped_effort(&m, "medium"), None);
    }

    #[test]
    fn compaction_uses_low_independent_of_live_effort() {
        use crate::provider::ThinkingLevel;

        for active in [None, Some(ThinkingLevel::Off), Some(ThinkingLevel::Max)] {
            assert_eq!(
                compaction_thinking(&json!({ "reasoning": true }), active),
                Some(ThinkingLevel::Low)
            );
        }
        assert_eq!(
            compaction_thinking(&Value::Null, Some(ThinkingLevel::High)),
            Some(ThinkingLevel::Low)
        );
        assert_eq!(compaction_thinking(&Value::Null, None), None);
        assert_eq!(
            compaction_thinking(&Value::Null, Some(ThinkingLevel::Off)),
            None
        );
        assert_eq!(
            compaction_thinking(&json!({ "reasoning": false }), Some(ThinkingLevel::High)),
            None
        );
    }

    #[test]
    fn compaction_clamps_low_using_model_metadata() {
        use crate::provider::ThinkingLevel;

        assert_eq!(
            compaction_thinking(
                &json!({
                    "reasoning": true,
                    "thinkingLevelMap": { "off": null, "minimal": null, "low": null, "medium": null, "high": "high" }
                }),
                None
            ),
            Some(ThinkingLevel::High)
        );
        assert_eq!(
            compaction_thinking(
                &json!({
                    "compat": {
                        "reasoning": true,
                        "thinkingLevelMap": { "off": "off", "minimal": null, "low": null, "medium": null, "high": null, "xhigh": null, "max": null }
                    }
                }),
                None
            ),
            None
        );
        assert_eq!(
            compaction_thinking(&json!({ "reasoning": true, "thinkingLevelMap": {} }), None),
            Some(ThinkingLevel::Low)
        );
    }

    #[test]
    fn merge_compat_overrides_per_key() {
        let merged = merge_compat(
            Some(&json!({ "supportsStore": false, "thinkingFormat": "deepseek" })),
            Some(&json!({ "supportsStore": true })),
        );
        assert_eq!(merged["supportsStore"], json!(true));
        assert_eq!(merged["thinkingFormat"], json!("deepseek"));
        assert_eq!(merge_compat(None, None), Value::Null);
    }

    #[test]
    fn level_map_reads_top_level_and_nested_compat() {
        let top = json!({ "thinkingLevelMap": { "max": "max" } });
        assert_eq!(
            parse_level_map(&top).unwrap().get("max"),
            Some(&json!("max"))
        );
        // whole model-entry shape: nested compat plus a top-level map; the
        // top-level map wins.
        let mixed = json!({
            "thinkingLevelMap": { "max": "max" },
            "compat": { "forceAdaptiveThinking": true, "thinkingLevelMap": { "xhigh": "xhigh" } },
        });
        assert_eq!(
            parse_level_map(&mixed).unwrap().get("max"),
            Some(&json!("max"))
        );
        assert_eq!(get_bool(&mixed, "forceAdaptiveThinking"), Some(true));
        // nested-only map (bare compat object)
        let nested = json!({ "compat": { "thinkingLevelMap": { "xhigh": "xhigh" } } });
        assert_eq!(
            parse_level_map(&nested).unwrap().get("xhigh"),
            Some(&json!("xhigh"))
        );
    }
}
