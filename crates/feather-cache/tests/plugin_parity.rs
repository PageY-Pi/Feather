//! Parity tests for the pure `pi-cache-optimizer` ports in
//! `feather_cache::plugin`, driven by fixtures that mirror the request shapes
//! feather's providers actually serialise.

use feather_cache::plugin::*;
use serde_json::{json, Value};

fn fixture(name: &str) -> Value {
    let raw = match name {
        "short_before_long" => {
            include_str!("fixtures/anthropic_ttl_short_before_long.json")
        }
        "long_then_short" => {
            include_str!("fixtures/anthropic_ttl_long_then_short.json")
        }
        "long_only" => include_str!("fixtures/anthropic_long_only.json"),
        "openai" => {
            include_str!("fixtures/openai_payload_with_key_and_retention.json")
        }
        other => panic!("unknown fixture {other}"),
    };
    serde_json::from_str(raw).unwrap()
}

#[test]
fn anthropic_cache_control_matches_pi_get_cache_control() {
    assert_eq!(anthropic_cache_control(CacheRetention::None, true), None);
    assert_eq!(
        anthropic_cache_control(CacheRetention::Short, true),
        Some(json!({"type": "ephemeral"}))
    );
    assert_eq!(
        anthropic_cache_control(CacheRetention::Long, true),
        Some(json!({"type": "ephemeral", "ttl": "1h"}))
    );
    // long without endpoint support stays on the default window
    assert_eq!(
        anthropic_cache_control(CacheRetention::Long, false),
        Some(json!({"type": "ephemeral"}))
    );
}

#[test]
fn anthropic_ttl_wire_order_conflicts_match_plugin() {
    let short_before_long = fixture("short_before_long");
    assert!(has_anthropic_cache_ttl_order_conflict(&short_before_long));
    assert!(has_anthropic_long_cache_control(&short_before_long));

    let long_then_short = fixture("long_then_short");
    assert!(!has_anthropic_cache_ttl_order_conflict(&long_then_short));
    assert!(has_anthropic_long_cache_control(&long_then_short));

    let long_only = fixture("long_only");
    assert!(!has_anthropic_cache_ttl_order_conflict(&long_only));
    assert!(has_anthropic_long_cache_control(&long_only));

    // empty / non-array payloads are inert
    assert!(!has_anthropic_cache_ttl_order_conflict(&json!({})));
    assert!(!has_anthropic_cache_ttl_order_conflict(
        &json!({"messages": 3})
    ));
}

#[test]
fn anthropic_ttl_repair_returns_copy_and_never_mutates_input() {
    let conflict = fixture("short_before_long");
    let original = conflict.clone();
    let repaired = with_anthropic_cache_ttl_repair(&conflict, false).expect("needs repair");
    assert_eq!(conflict, original, "input must be untouched");
    assert!(!has_anthropic_long_cache_control(&repaired));
    assert!(!has_anthropic_cache_ttl_order_conflict(&repaired));

    // already-valid payload: no repair copy
    let valid = fixture("long_then_short");
    assert!(with_anthropic_cache_ttl_repair(&valid, false).is_none());

    // force_downgrade applies the process-local fallback to long-only payloads
    let long_only = fixture("long_only");
    let forced = with_anthropic_cache_ttl_repair(&long_only, true).expect("forced repair");
    assert!(!has_anthropic_long_cache_control(&forced));
}

#[test]
fn anthropic_ttl_in_place_downgrade_is_idempotent() {
    let mut payload = fixture("short_before_long");
    assert!(downgrade_anthropic_long_cache_controls(&mut payload));
    assert!(!has_anthropic_long_cache_control(&payload));
    assert!(!downgrade_anthropic_long_cache_controls(&mut payload));
}

#[test]
fn anthropic_ttl_order_error_recognition() {
    let message = json!({
        "role": "assistant",
        "stopReason": "error",
        "errorMessage": "cache_control: a ttl='1h' breakpoint must not come after a ttl='5m' breakpoint"
    });
    assert!(has_anthropic_cache_ttl_order_error(&message));
    assert!(!has_anthropic_cache_ttl_order_error(&json!({
        "role": "user",
        "errorMessage": "cache_control ttl='1h' ttl='5m' must not come after"
    })));
    assert!(!has_anthropic_cache_ttl_order_error(&json!({
        "role": "assistant",
        "stopReason": "stop",
        "errorMessage": "cache_control ttl='1h' ttl='5m' must not come after"
    })));
}

#[test]
fn retention_gate_matches_plugin_precedence() {
    assert_eq!(resolve_cache_retention(None, false), CacheRetention::Short);
    assert_eq!(resolve_cache_retention(None, true), CacheRetention::Long);
    assert_eq!(
        resolve_cache_retention(Some(CacheRetention::None), true),
        CacheRetention::None
    );

    let case = |provider_rejected, official_openai, explicit_opt_in, user_requested_long| {
        decide_prompt_cache_retention(PromptCacheRetentionGateInput {
            provider_rejected,
            official_openai,
            explicit_opt_in,
            user_requested_long,
        })
    };
    assert_eq!(
        case(true, true, true, true).reason,
        PromptCacheRetentionReason::ProviderRejected
    );
    assert!(case(false, true, false, false).keep);
    assert_eq!(
        case(false, false, true, false).reason,
        PromptCacheRetentionReason::ExplicitOptIn
    );
    assert_eq!(
        case(false, false, false, true).reason,
        PromptCacheRetentionReason::UserRequestedLong
    );
    let unverified = case(false, false, false, false);
    assert!(!unverified.keep);
    assert_eq!(
        unverified.reason,
        PromptCacheRetentionReason::UnverifiedEndpoint
    );
}

#[test]
fn prompt_cache_key_helpers_match_plugin() {
    let long = "x".repeat(80);
    assert_eq!(
        clamp_prompt_cache_key(Some(&long)).unwrap().chars().count(),
        64
    );
    assert_eq!(clamp_prompt_cache_key(Some("  ab  ")), Some("ab".into()));
    assert_eq!(clamp_prompt_cache_key(Some("   ")), None);
    assert_eq!(clamp_prompt_cache_key(None), None);

    let payload = json!({"model": "m", "messages": []});
    let added = add_openai_prompt_cache_key(&payload, Some("sess-1")).unwrap();
    assert_eq!(added["prompt_cache_key"], json!("sess-1"));
    // an effective key is never overwritten
    assert!(add_openai_prompt_cache_key(&added, Some("other")).is_none());
    assert!(add_openai_prompt_cache_key(&json!({"promptCacheKey": "keep"}), Some("x")).is_none());
}

#[test]
fn openai_payload_fixture_helpers() {
    let payload = fixture("openai");
    assert!(has_effective_prompt_cache_key(payload.as_object().unwrap()));

    let omitted = omit_openai_prompt_cache_keys(&payload).expect("key present");
    assert!(omitted.get("prompt_cache_key").is_none());
    assert!(omitted.get("promptCacheKey").is_none());
    assert_eq!(
        payload["prompt_cache_key"],
        "pi-0123456789abcdef0123456789abcdef"
    );

    let stripped = without_prompt_cache_retention(&payload).expect("retention present");
    assert!(stripped.get("prompt_cache_retention").is_none());
    assert!(without_prompt_cache_retention(&json!({"messages": []})).is_none());
    assert!(omit_openai_prompt_cache_keys(&json!({"messages": []})).is_none());
}

#[test]
fn derived_session_key_matches_provider_derivation() {
    let ours = derive_prompt_cache_key(Some("session-abc")).unwrap();
    let provider = feather_core::provider::openai::derive_prompt_cache_key("session-abc");
    assert_eq!(
        ours, provider,
        "optimizer key must equal the provider's key"
    );
    assert!(ours.starts_with("pi-"));
    assert_eq!(ours.len(), 3 + 32);
    assert!(!ours.contains("session-abc"), "raw id must not leak");
    assert_eq!(derive_prompt_cache_key(Some("  ")), None);
    assert_eq!(derive_prompt_cache_key(None), None);
    assert_ne!(
        derive_prompt_cache_key(Some("a")),
        derive_prompt_cache_key(Some("b"))
    );
}
