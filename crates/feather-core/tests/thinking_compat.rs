//! Focused request-body tests for model-specific thinking support and the
//! optional provider `compat` metadata.
//!
//! Anthropic: adaptive Claude 4.6+ effort mapping + non-adaptive budget
//! fallback + temperature incompatibility.
//! OpenAI-compatible: custom `compat` (`thinkingLevelMap`,
//! `supportsReasoningEffort`, `maxTokensField`, `supportsStore`,
//! `supportsDeveloperRole`, `thinkingFormat`).

use feather_core::{
    message::Message,
    provider::{
        anthropic::AnthropicProvider, compat::merge_compat, openai::OpenAiProvider, CacheHints,
        ChatRequest, ThinkingLevel,
    },
};
use serde_json::{json, Value};

fn request(
    model: &str,
    max_tokens: u32,
    temperature: Option<f32>,
    thinking: Option<ThinkingLevel>,
) -> ChatRequest {
    ChatRequest {
        system: "sys".into(),
        messages: vec![Message::user_text("hi")].into(),
        tools: vec![].into(),
        model: model.into(),
        max_tokens,
        temperature,
        cache: CacheHints::default(),
        thinking,
        session_id: String::new(),
    }
}

#[test]
fn volcengine_compat_uses_native_thinking_toggle_and_off_map() {
    // GLM-5.3 family: forced thinking, so `off` is unmapped and never sent as a
    // disable request; low..max are the real efforts.
    let glm = json!({
        "reasoning": true,
        "thinkingFormat": "deepseek",
        "supportsReasoningEffort": true,
        "maxTokensField": "max_tokens",
        "thinkingLevelMap": {
            "off": null,
            "minimal": null,
            "low": "low",
            "medium": "medium",
            "high": "high",
            "xhigh": "xhigh",
            "max": "max"
        }
    });
    let off = openai("glm-5.3-flash", Some(ThinkingLevel::Off), &glm);
    assert!(off.get("thinking").is_none(), "{off}");
    assert!(off.get("reasoning_effort").is_none(), "{off}");

    let high = openai("glm-5.3-flash", Some(ThinkingLevel::High), &glm);
    assert_eq!(high["thinking"]["type"], json!("enabled"));
    assert_eq!(high["reasoning_effort"], json!("high"));
    assert_eq!(high["max_tokens"], json!(4096));

    let max = openai("glm-5.3-flash", Some(ThinkingLevel::Max), &glm);
    assert_eq!(max["reasoning_effort"], json!("max"));

    // Every other coding-plan model keeps the disable toggle.
    let convertible = json!({
        "reasoning": true,
        "thinkingFormat": "deepseek",
        "supportsReasoningEffort": true,
        "maxTokensField": "max_tokens",
        "thinkingLevelMap": {
            "off": "none",
            "minimal": null,
            "low": "low",
            "medium": "medium",
            "high": "high",
            "xhigh": "xhigh",
            "max": "max"
        }
    });
    let off = openai("deepseek-v4-pro", Some(ThinkingLevel::Off), &convertible);
    assert_eq!(off["thinking"]["type"], json!("disabled"));
    assert!(off.get("reasoning_effort").is_none(), "{off}");

    let xhigh = openai("deepseek-v4-pro", Some(ThinkingLevel::Xhigh), &convertible);
    assert_eq!(xhigh["thinking"]["type"], json!("enabled"));
    assert_eq!(xhigh["reasoning_effort"], json!("xhigh"));

    // `minimal` is unmapped on purpose: Ark normalises it to `none`, which the
    // models reject, so it clamps up to the lowest real effort instead.
    let minimal = openai(
        "deepseek-v4-pro",
        Some(ThinkingLevel::Minimal),
        &convertible,
    );
    assert_eq!(minimal["thinking"]["type"], json!("enabled"));
    assert_eq!(minimal["reasoning_effort"], json!("low"));

    // GLM has no real `minimal` either, and no `off` at all.
    let minimal = openai("glm-5.3-flash", Some(ThinkingLevel::Minimal), &glm);
    assert_eq!(minimal["thinking"]["type"], json!("enabled"));
    assert_eq!(minimal["reasoning_effort"], json!("low"));
}
fn anthropic(model: &str, thinking: ThinkingLevel, compat: &Value) -> Value {
    AnthropicProvider::build_body_with_compat(
        &request(model, 8192, Some(1.0), Some(thinking)),
        compat,
    )
}

fn openai(model: &str, thinking: Option<ThinkingLevel>, compat: &Value) -> Value {
    OpenAiProvider::build_body_with_compat(&request(model, 4096, Some(1.0), thinking), compat)
}

// ---------------------------------------------------------------------------
// Anthropic: adaptive Claude 4.6+
// ---------------------------------------------------------------------------

#[test]
fn claude_4_6_adaptive_uses_effort_and_omits_temperature() {
    for model in [
        "claude-opus-4-6",
        "claude-sonnet-4-6",
        "anthropic/claude-opus-4-6",
    ] {
        let body = anthropic(model, ThinkingLevel::High, &Value::Null);
        assert_eq!(body["thinking"]["type"], json!("adaptive"), "{model}");
        assert_eq!(body["thinking"]["display"], json!("summarized"), "{model}");
        assert_eq!(body["output_config"]["effort"], json!("high"), "{model}");
        assert!(body["thinking"].get("budget_tokens").is_none(), "{model}");
        assert!(
            body.get("temperature").is_none(),
            "{model}: temperature is incompatible with thinking"
        );
    }
}

#[test]
fn claude_4_6_effort_mapping_matches_pi_levels_and_clamping() {
    // pi: minimal/low -> low, medium -> medium, high -> high; xhigh is not in
    // the 4.6 map so `clampThinkingLevel` scans forward to `max`.
    let cases = [
        (ThinkingLevel::Minimal, "low"),
        (ThinkingLevel::Low, "low"),
        (ThinkingLevel::Medium, "medium"),
        (ThinkingLevel::High, "high"),
        (ThinkingLevel::Xhigh, "max"),
        (ThinkingLevel::Max, "max"),
    ];
    for (level, effort) in cases {
        let body = anthropic("claude-opus-4-6", level, &Value::Null);
        assert_eq!(body["output_config"]["effort"], json!(effort), "{level:?}");
    }
}

#[test]
fn claude_4_6_explicit_off_disables_thinking() {
    let body = anthropic("claude-opus-4-6", ThinkingLevel::Off, &Value::Null);
    assert_eq!(body["thinking"]["type"], json!("disabled"));
    // off on this model is not mapped to null, so the disabled block is sent
    assert!(body.get("output_config").is_none());
}

#[test]
fn adaptive_models_with_null_off_map_omit_the_disabled_block() {
    // pi: `thinkingLevelMap.off === null` -> no `thinking` field at all.
    let body = anthropic("claude-opus-5-5", ThinkingLevel::Off, &Value::Null);
    assert!(body.get("thinking").is_none());
}

#[test]
fn opus_4_7_and_later_never_send_temperature() {
    // `supportsTemperature: false` even when thinking is off/None.
    let body = AnthropicProvider::build_body(&request("claude-opus-4-7", 8192, Some(1.0), None));
    assert!(body.get("temperature").is_none());
    let body = anthropic("claude-opus-4-7", ThinkingLevel::High, &Value::Null);
    assert_eq!(body["thinking"]["type"], json!("adaptive"));
    assert!(body.get("temperature").is_none());
}

#[test]
fn compat_can_force_adaptive_on_an_unknown_model() {
    let compat = json!({
        "forceAdaptiveThinking": true,
        "thinkingLevelMap": { "max": "max" },
    });
    let body = anthropic(
        "vendor/custom-thinking-model",
        ThinkingLevel::Xhigh,
        &compat,
    );
    assert_eq!(body["thinking"]["type"], json!("adaptive"));
    assert_eq!(body["output_config"]["effort"], json!("max"));
    // unknown model defaults to temperature-capable, but thinking suppresses it
    assert!(body.get("temperature").is_none());
}

#[test]
fn compat_understands_a_whole_pi_model_entry_shape() {
    // pi model metadata keeps `thinkingLevelMap` at the top level next to a
    // nested `compat` object. The builder accepts either that shape or a bare
    // compat object.
    let compat = json!({
        "id": "vendor/custom-thinking-model",
        "thinkingLevelMap": { "max": "max" },
        "compat": { "forceAdaptiveThinking": true, "supportsTemperature": true },
    });
    let body = anthropic(
        "vendor/custom-thinking-model",
        ThinkingLevel::Xhigh,
        &compat,
    );
    assert_eq!(body["thinking"]["type"], json!("adaptive"));
    assert_eq!(body["output_config"]["effort"], json!("max"));
}

#[test]
fn catalog_adaptive_maps_and_effort_fallback_are_exact() {
    // Values mirror the installed pi-ai anthropic catalog.
    let cases = [
        ("claude-opus-4-6", ThinkingLevel::High, "high"),
        ("claude-opus-4-6", ThinkingLevel::Xhigh, "max"),
        ("claude-opus-4-6", ThinkingLevel::Max, "max"),
        ("claude-opus-4-6", ThinkingLevel::Minimal, "low"),
        ("claude-opus-4-7", ThinkingLevel::Xhigh, "xhigh"),
        ("claude-opus-4-7", ThinkingLevel::Medium, "medium"),
        ("claude-sonnet-5", ThinkingLevel::Xhigh, "xhigh"),
        ("claude-sonnet-5", ThinkingLevel::Low, "low"),
        ("claude-opus-5-5", ThinkingLevel::Minimal, "low"),
        ("claude-opus-5-5", ThinkingLevel::High, "high"),
        ("claude-opus-5-5", ThinkingLevel::Xhigh, "xhigh"),
    ];
    for (model, level, effort) in cases {
        let body = anthropic(model, level, &Value::Null);
        assert_eq!(
            body["output_config"]["effort"],
            json!(effort),
            "{model} {level:?}"
        );
        assert_eq!(
            body["thinking"]["type"],
            json!("adaptive"),
            "{model} {level:?}"
        );
    }
}

#[test]
fn openai_nested_compat_and_top_level_map_are_understood() {
    let compat = json!({
        "thinkingLevelMap": {
            "minimal": null, "low": "low", "medium": null, "high": "high", "max": "max"
        },
        "compat": {
            "thinkingFormat": "deepseek",
            "supportsStore": false,
            "maxTokensField": "max_tokens"
        }
    });
    let body = openai("deepseek-flash", Some(ThinkingLevel::Medium), &compat);
    assert_eq!(body["thinking"]["type"], json!("enabled"));
    assert_eq!(body["reasoning_effort"], json!("high"));
    assert_eq!(body["max_tokens"], json!(4096));
}

// ---------------------------------------------------------------------------
// Anthropic: non-adaptive budget fallback / adjustment / clamping
// ---------------------------------------------------------------------------

#[test]
fn nonadaptive_budget_clamps_to_answer_room() {
    // High budget is 16384, clamped to max_tokens - 1024.
    let body = anthropic("claude-opus-4-5", ThinkingLevel::High, &Value::Null);
    assert_eq!(body["thinking"]["type"], json!("enabled"));
    assert_eq!(body["thinking"]["budget_tokens"], json!(7168));
    assert_eq!(body["max_tokens"], json!(8192));
}

#[test]
fn nonadaptive_budget_uses_model_cap_when_compat_provides_it() {
    // pi `adjustMaxTokensForThinking`: maxTokens = min(base + budget, modelMax).
    let compat = json!({ "maxTokens": 64000 });
    let body = anthropic("claude-opus-4-5", ThinkingLevel::High, &compat);
    assert_eq!(body["max_tokens"], json!(24576)); // 8192 + 16384, under the cap
    assert_eq!(body["thinking"]["budget_tokens"], json!(16384));
    // A tight model cap clamps the expanded ceiling and the budget.
    let tight = json!({ "maxTokens": 10000 });
    let body = anthropic("claude-opus-4-5", ThinkingLevel::High, &tight);
    assert_eq!(body["max_tokens"], json!(10000));
    assert_eq!(body["thinking"]["budget_tokens"], json!(8976)); // 10000 - 1024
}

#[test]
fn compat_can_turn_off_adaptive_for_a_claude_4_6_model() {
    let compat = json!({ "forceAdaptiveThinking": false });
    let body = anthropic("claude-opus-4-6", ThinkingLevel::High, &compat);
    assert_eq!(body["thinking"]["type"], json!("enabled"));
    assert_eq!(body["thinking"]["budget_tokens"], json!(7168));
    assert!(body.get("output_config").is_none());
}

// ---------------------------------------------------------------------------
// OpenAI-compatible: custom compat
// ---------------------------------------------------------------------------

/// The deepseek catalog compat + thinkingLevelMap used by pi.
fn deepseek_compat() -> Value {
    json!({
        "supportsStore": false,
        "supportsDeveloperRole": false,
        "maxTokensField": "max_tokens",
        "thinkingFormat": "deepseek",
        "thinkingLevelMap": {
            "minimal": null, "low": "low", "medium": null, "high": "high", "max": "max"
        }
    })
}

#[test]
fn openai_deepseek_compat_matches_pi_body_shape() {
    let body = openai(
        "deepseek-flash",
        Some(ThinkingLevel::High),
        &deepseek_compat(),
    );
    assert_eq!(body["thinking"]["type"], json!("enabled"));
    assert_eq!(body["reasoning_effort"], json!("high"));
    assert_eq!(body["max_tokens"], json!(4096));
    assert!(body.get("max_completion_tokens").is_none());
    assert!(
        body.get("store").is_none(),
        "supportsStore=false must not send store"
    );
    assert_eq!(body["messages"][0]["role"], json!("system"));
    assert_eq!(body["temperature"], json!(1.0));
}

#[test]
fn openai_thinking_level_map_clamps_like_pi() {
    // medium -> null in the map, so pi clamps forward to high
    let body = openai(
        "deepseek-flash",
        Some(ThinkingLevel::Medium),
        &deepseek_compat(),
    );
    assert_eq!(body["reasoning_effort"], json!("high"));
    // minimal -> null, clamped forward to low
    let body = openai(
        "deepseek-flash",
        Some(ThinkingLevel::Minimal),
        &deepseek_compat(),
    );
    assert_eq!(body["reasoning_effort"], json!("low"));
}

#[test]
fn openai_deepseek_off_disables_and_drops_effort() {
    let body = openai(
        "deepseek-flash",
        Some(ThinkingLevel::Off),
        &deepseek_compat(),
    );
    assert_eq!(body["thinking"]["type"], json!("disabled"));
    assert!(body.get("reasoning_effort").is_none());
}

#[test]
fn openai_custom_compat_supports_store_developer_role_and_max_completion_tokens() {
    let compat = json!({
        "supportsStore": true,
        "supportsDeveloperRole": true,
        "supportsReasoningEffort": true,
        "maxTokensField": "max_completion_tokens",
        "reasoning": true,
    });
    let body = openai("gpt-x", Some(ThinkingLevel::High), &compat);
    assert_eq!(body["store"], json!(false));
    assert_eq!(body["messages"][0]["role"], json!("developer"));
    assert_eq!(body["max_completion_tokens"], json!(4096));
    assert!(body.get("max_tokens").is_none());
    assert_eq!(body["reasoning_effort"], json!("high"));
}

#[test]
fn openai_supports_reasoning_effort_false_omits_effort() {
    let compat = json!({ "supportsReasoningEffort": false, "reasoning": true });
    let body = openai("gpt-x", Some(ThinkingLevel::High), &compat);
    assert!(body.get("reasoning_effort").is_none());
}

#[test]
fn openai_openrouter_format_uses_nested_reasoning_object() {
    let compat = json!({
        "thinkingFormat": "openrouter",
        "thinkingLevelMap": { "off": null, "high": "high", "max": "max" },
    });
    let body = openai("vendor/model", Some(ThinkingLevel::High), &compat);
    assert_eq!(body["reasoning"]["effort"], json!("high"));
    assert!(body.get("reasoning_effort").is_none());
    // off undefined in the map -> pi clamps to off and sends the "none" default
    let compat = json!({ "thinkingFormat": "openrouter", "thinkingLevelMap": { "high": "high" } });
    let body = openai("vendor/model", Some(ThinkingLevel::Off), &compat);
    assert_eq!(body["reasoning"]["effort"], json!("none"));
    // off mapped to a string -> that string
    let compat = json!({ "thinkingFormat": "openrouter", "thinkingLevelMap": { "off": "none" } });
    let body = openai("vendor/model", Some(ThinkingLevel::Off), &compat);
    assert_eq!(body["reasoning"]["effort"], json!("none"));
}

#[test]
fn openai_default_keeps_direct_effort_passthrough() {
    // No compat: feather's existing minimal..max passthrough (existing tests
    // depend on this; pi's clamping only applies to a supplied map).
    let body = openai("gpt-x", Some(ThinkingLevel::Xhigh), &Value::Null);
    assert_eq!(body["reasoning_effort"], json!("xhigh"));
    let body = openai("gpt-x", Some(ThinkingLevel::Off), &Value::Null);
    assert!(body.get("reasoning_effort").is_none());
}

#[test]
fn merge_compat_lets_model_override_provider_keys() {
    // Guidance for the parent: provider compat merged with model compat.
    let merged = merge_compat(
        Some(&json!({ "maxTokensField": "max_completion_tokens", "supportsStore": true })),
        Some(&json!({ "maxTokensField": "max_tokens" })),
    );
    assert_eq!(merged["maxTokensField"], json!("max_tokens"));
    assert_eq!(merged["supportsStore"], json!(true));
    let body = OpenAiProvider::build_body_with_compat(
        &request("vendor/model", 4096, Some(1.0), Some(ThinkingLevel::High)),
        &merged,
    );
    assert_eq!(body["max_tokens"], json!(4096));
    assert_eq!(body["store"], json!(false));
}
