//! Anthropic Messages API provider (streaming, prompt-caching aware).
//!
//! Native `/v1/messages` protocol: system as a content array (so the
//! cache-optimizer can attach `cache_control`), tools with a trailing
//! `cache_control` breakpoint, `tool_result` blocks wrapped in user turns.

use super::compat::{clamp_level, get_bool, get_u32, mapped_effort, parse_level_map};
use super::{
    gen_id, http_post_sse, ChatRequest, EventHandler, ProviderError, ResponseStream, StreamEvent,
    ToolDef,
};
use crate::message::{ContentBlock, Message, Role, StopReason, Usage};
use crate::provider::ThinkingLevel;
use serde_json::{json, Value};
use std::collections::BTreeMap;

pub const ANTHROPIC_VERSION: &str = "2023-06-01";
pub const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";

pub struct AnthropicProvider {
    client: reqwest::Client,
    credentials: super::credentials::CredentialSource,
    base_url: String,
    /// Provider/model `compat` metadata handed in by the caller (pi catalog
    /// shape). Empty for the plain constructors; see [`Self::with_compat`].
    compat: Value,
}

impl AnthropicProvider {
    pub fn new(api_key: impl Into<String>, base_url: Option<String>) -> Self {
        Self {
            client: super::shared_client(),
            credentials: super::credentials::CredentialSource::new(api_key),
            base_url: base_url.unwrap_or_else(|| DEFAULT_BASE_URL.to_string()),
            compat: Value::Null,
        }
    }

    /// Attach pi-style compatibility metadata (`compat`) for the selected
    /// provider/model. The parent passes the catalog entry's `compat` value;
    /// absent/`Null` keeps the static model-ID defaults. Chainable so existing
    /// `new(...)` call sites stay source-compatible.
    pub fn with_compat(mut self, compat: Value) -> Self {
        self.compat = compat;
        self
    }

    pub fn with_credentials(
        mut self,
        resolver: std::sync::Arc<dyn super::credentials::CredentialResolver>,
    ) -> Self {
        self.credentials = self.credentials.with_resolver(resolver);
        self
    }

    /// Convert the linear history into Anthropic message turns.
    /// Convert the linear history into Anthropic message turns.
    ///
    /// Consecutive user/tool_result messages (e.g. results of parallel tool
    /// calls) and consecutive assistant messages are merged into single turns,
    /// as the Messages API expects; `converted_index` maps back into the
    /// merged list.
    pub fn convert_messages(messages: &[Message]) -> Vec<Value> {
        let mut out: Vec<Value> = Vec::new();
        for m in messages {
            match m.role {
                Role::User | Role::ToolResult => {
                    let blocks: Vec<Value> = match m.role {
                        Role::ToolResult => {
                            // Anthropic rejects empty text blocks; an empty
                            // tool output (grep miss, empty read) is common
                            let text = m.text();
                            let text = if text.is_empty() {
                                "(no output)".to_string()
                            } else {
                                text
                            };
                            vec![json!({
                                "type": "tool_result",
                                "tool_use_id": m.tool_call_id.clone().unwrap_or_default(),
                                "content": [{ "type": "text", "text": text }],
                                "is_error": m.is_error,
                            })]
                        }
                        _ => m.content.iter().filter_map(|b| match b {
                            ContentBlock::Text { text } if !text.is_empty() => Some(json!({ "type": "text", "text": text })),
                            ContentBlock::Image { media_type, data } => Some(json!({
                                "type": "image", "source": { "type": "base64", "media_type": media_type, "data": data }
                            })),
                            _ => None,
                        }).collect(),
                    };
                    match out.last_mut() {
                        Some(last) if last.get("role").and_then(|r| r.as_str()) == Some("user") => {
                            if let Some(content) =
                                last.get_mut("content").and_then(|c| c.as_array_mut())
                            {
                                content.extend(blocks);
                            }
                        }
                        _ => out.push(json!({ "role": "user", "content": blocks })),
                    }
                }
                Role::Assistant => {
                    let mut content: Vec<Value> = Vec::new();
                    for b in &m.content {
                        match b {
                            ContentBlock::Thinking { text, signature } => {
                                let mut blk = json!({ "type": "thinking", "thinking": text });
                                if let Some(sig) = signature {
                                    blk["signature"] = json!(sig);
                                }
                                content.push(blk);
                            }
                            ContentBlock::Text { text } => {
                                if !text.is_empty() {
                                    content.push(json!({ "type": "text", "text": text }));
                                }
                            }
                            ContentBlock::Image { .. } => {} // images ride in user turns only
                            ContentBlock::ToolCall(c) => {
                                content.push(json!({
                                    "type": "tool_use",
                                    "id": c.id,
                                    "name": c.name,
                                    "input": c.arguments,
                                }));
                            }
                        }
                    }
                    if content.is_empty() {
                        content.push(json!({ "type": "text", "text": "(empty response)" }));
                    }
                    match out.last_mut() {
                        Some(last)
                            if last.get("role").and_then(|r| r.as_str()) == Some("assistant") =>
                        {
                            if let Some(existing) =
                                last.get_mut("content").and_then(|c| c.as_array_mut())
                            {
                                existing.extend(content);
                            }
                        }
                        _ => out.push(json!({ "role": "assistant", "content": content })),
                    }
                }
                Role::System => {} // never sent inline
            }
        }
        out
    }

    /// Index of feather message `target` inside the `convert_messages` output
    /// (consecutive same-side messages merge, so indexes are not 1:1). Used to
    /// place the cache breakpoint at the end of the stable prefix.
    pub fn converted_index(messages: &[Message], target: usize) -> Option<usize> {
        let mut groups = 0usize;
        let mut prev_user: Option<bool> = None;
        for (i, m) in messages.iter().enumerate() {
            let is_user = match m.role {
                Role::User | Role::ToolResult => true,
                Role::Assistant => false,
                Role::System => continue, // skipped by convert_messages
            };
            if prev_user != Some(is_user) {
                groups += 1;
            }
            prev_user = Some(is_user);
            if i == target {
                return Some(groups - 1);
            }
        }
        None
    }

    /// Resolve the effective compat for a request from the static model-ID
    /// metadata plus any catalog `compat` handed to the provider. pi keeps this
    /// on the model; `ChatRequest` must not grow fields, so the model ID is the
    /// static signal and the builder's `compat` overrides it per key.
    fn resolve_model_compat(model: &str, compat: &Value) -> AnthropicModelCompat {
        let mut meta = anthropic_model_compat(model);
        if let Some(v) = get_bool(compat, "forceAdaptiveThinking") {
            meta.force_adaptive_thinking = v;
        }
        if let Some(v) = get_bool(compat, "supportsTemperature") {
            meta.supports_temperature = v;
        }
        if let Some(map) = parse_level_map(compat) {
            meta.thinking_level_map = map;
        }
        meta
    }

    /// Assemble the full request body, honoring cache-optimizer hints.
    pub fn build_body(req: &ChatRequest) -> Value {
        Self::build_body_with_compat(req, &Value::Null)
    }

    /// Compat-aware body assembly. `compat` is the caller-supplied pi catalog
    /// object; `Value::Null` reproduces the static model-ID defaults.
    pub fn build_body_with_compat(req: &ChatRequest, compat: &Value) -> Value {
        let meta = Self::resolve_model_compat(&req.model, compat);
        // One retention policy across tools/system/messages preserves wire-order TTL validity.
        let mut cache_control = json!({"type":"ephemeral"});
        if req.cache.sticky_ttl_seconds.is_some_and(|ttl| ttl >= 3600)
            && get_bool(compat, "supportsLongCacheRetention").unwrap_or(true)
        {
            cache_control["ttl"] = json!("1h");
        }
        let mut system_blk = json!({ "type": "text", "text": req.system });
        if req.cache.system {
            system_blk["cache_control"] = cache_control.clone();
        }
        let system_block = if req.system.is_empty() {
            None // empty text blocks are rejected outright
        } else {
            Some(system_blk)
        };
        let mut tools: Vec<Value> = req
            .tools
            .iter()
            .map(|t: &ToolDef| {
                json!({
                    "name": t.name,
                    "description": t.description,
                    "input_schema": t.parameters,
                })
            })
            .collect();
        if req.cache.tools {
            if let Some(last) = tools.last_mut() {
                last["cache_control"] = cache_control.clone();
            }
        }
        let mut messages = Self::convert_messages(&req.messages);
        if let Some(idx) = req
            .cache
            .stable_message_index
            .and_then(|i| Self::converted_index(&req.messages, i))
        {
            let last_i = messages.len().saturating_sub(1);
            if let Some(msg) = messages.get_mut(idx.min(last_i)) {
                // attach a breakpoint on the last content block of the
                // stable-prefix message
                if let Some(content) = msg.get_mut("content").and_then(|c| c.as_array_mut()) {
                    if let Some(blk) = content.last_mut() {
                        blk["cache_control"] = cache_control.clone();
                    }
                }
            }
        }
        let thinking_enabled = matches!(req.thinking, Some(l) if l != ThinkingLevel::Off);
        let mut body = json!({
            "model": req.model,
            "max_tokens": req.max_tokens,
            "stream": true,
            "system": system_block,
            "tools": tools,
            "messages": messages,
        });
        if system_block.is_none() {
            // `"system": null` is itself rejected — omit the field entirely
            if let Some(obj) = body.as_object_mut() {
                obj.remove("system");
            }
        }
        // Temperature is incompatible with extended thinking (pi) and
        // unsupported on Opus 4.7+ / managed-effort models
        // (`supportsTemperature: false`).
        if let Some(t) = req.temperature {
            if !thinking_enabled && meta.supports_temperature {
                body["temperature"] = json!(t);
            }
        }
        if thinking_enabled {
            let level = req.thinking.unwrap_or(ThinkingLevel::High);
            if meta.force_adaptive_thinking {
                // Adaptive Claude (4.6+): Claude picks the depth; feather maps
                // the requested level to an `effort` after pi level clamping.
                let clamped = clamp_level(&meta.thinking_level_map, level.as_str());
                let effort = map_thinking_level_to_effort(&meta.thinking_level_map, &clamped);
                body["thinking"] = json!({ "type": "adaptive", "display": "summarized" });
                body["output_config"] = json!({ "effort": effort });
            } else {
                // Older models: budget-based thinking. pi adds the budget to the
                // response ceiling and keeps 1024 tokens for the answer; the
                // model cap is only known when compat carries `maxTokens`.
                let (max_tokens, budget) = adjust_max_tokens_for_thinking(
                    req.max_tokens,
                    get_u32(compat, "maxTokens"),
                    level,
                );
                body["max_tokens"] = json!(max_tokens);
                if budget > 0 {
                    body["thinking"] = json!({ "type": "enabled", "budget_tokens": budget, "display": "summarized" });
                }
            }
        } else if req.thinking == Some(ThinkingLevel::Off)
            && !matches!(meta.thinking_level_map.get("off"), Some(Value::Null))
        {
            // pi omits the disabled block when the model maps `off` to null.
            body["thinking"] = json!({ "type": "disabled" });
        }
        body
    }

    pub async fn stream(&self, req: &ChatRequest) -> Result<ResponseStream, ProviderError> {
        if req.thinking.is_some_and(|l| l != ThinkingLevel::Off) && req.max_tokens < 2048 {
            return Err(ProviderError::Decode(
                "Anthropic thinking needs max_tokens >= 2048 (1024 thinking + answer room)".into(),
            ));
        }
        let cred = self.credentials.resolve().await?;
        let base = cred.base_url.as_deref().unwrap_or(&self.base_url);
        // Catalogs may include /v1; never produce /v1/v1/messages.
        let base = base.trim_end_matches('/');
        let url = if base.ends_with("/v1") {
            format!("{base}/messages")
        } else {
            format!("{base}/v1/messages")
        };
        let oauth = cred.api_key.starts_with("sk-ant-oat");
        let mut headers = cred.request_headers(req);
        headers.push(("anthropic-version".into(), ANTHROPIC_VERSION.into()));
        if oauth || cred.bearer_auth {
            headers.push(("authorization".into(), format!("Bearer {}", cred.api_key)));
        } else {
            headers.push(("x-api-key".into(), cred.api_key));
        }
        let mut body = Self::build_body_with_compat(req, &self.compat);
        // The Messages API requires a string or array, not a single block object.
        if let Some(system) = body.get_mut("system") {
            if system.is_object() {
                *system = json!([system.clone()]);
            }
        }
        if oauth {
            headers.extend([
                ("anthropic-beta".into(), "claude-code-20250219,oauth-2025-04-20".into()),
                ("user-agent".into(), "claude-cli/2.1.280".into()),
                ("x-app".into(), "cli".into()),
                ("anthropic-dangerous-direct-browser-access".into(), "true".into()),
            ]);
            apply_oauth_identity(&mut body, req.cache.system);
        }
        let tool_names: Vec<String> = if oauth { req.tools.iter().map(|t| t.name.clone()).collect() } else { Vec::new() };
        let handler: EventHandler = Box::new(move |payload, out| {
            let start = out.len();
            anthropic_event(payload, out);
            if oauth {
                for event in &mut out[start..] {
                    if let StreamEvent::ToolCallStart { name, .. } = event {
                        if let Some(original) = tool_names.iter().find(|t| t.eq_ignore_ascii_case(name)) {
                            *name = original.clone();
                        }
                    }
                }
            }
        });
        http_post_sse(&self.client, &url, headers, body, handler).await
    }
}

/// Match the public Claude OAuth client request contract, as pi does.
fn apply_oauth_identity(body: &mut Value, cache_system: bool) {
    let mut identity = json!({
        "type": "text",
        "text": "You are Claude Code, Anthropic's official CLI for Claude."
    });
    if cache_system {
        let policy = body.pointer("/system/0/cache_control").cloned()
            .unwrap_or_else(|| json!({"type":"ephemeral"}));
        identity["cache_control"] = policy;
    }
    let system = body.as_object_mut().unwrap().entry("system").or_insert_with(|| json!([]));
    system.as_array_mut().unwrap().insert(0, identity);
    if let Some(tools) = body.get_mut("tools").and_then(Value::as_array_mut) {
        for tool in tools {
            if let Some(name) = tool.get_mut("name") {
                if let Some(s) = name.as_str() { *name = json!(claude_tool_name(s)); }
            }
        }
    }
    if let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut) {
        for message in messages {
            if let Some(blocks) = message.get_mut("content").and_then(Value::as_array_mut) {
                for block in blocks {
                    if block["type"] == "tool_use" {
                        if let Some(s) = block["name"].as_str() {
                            block["name"] = json!(claude_tool_name(s));
                        }
                    }
                }
            }
        }
    }
}

fn claude_tool_name(name: &str) -> String {
    const NAMES: &[&str] = &["Read", "Write", "Edit", "Bash", "Grep", "Glob", "AskUserQuestion",
        "EnterPlanMode", "ExitPlanMode", "KillShell", "NotebookEdit", "Skill", "Task", "TaskOutput",
        "TodoWrite", "WebFetch", "WebSearch"];
    NAMES.iter().find(|s| s.eq_ignore_ascii_case(name)).copied().unwrap_or(name).to_string()
}

/// Static per-model Anthropic thinking metadata, keyed by model ID. pi keeps
/// this in its catalog (`compat.forceAdaptiveThinking`, `thinkingLevelMap`,
/// `compat.supportsTemperature`); feather derives it from the model ID so a
/// plain `ChatRequest` needs no new fields, and lets builder `compat` override
/// it per key.
#[derive(Debug, Clone)]
pub struct AnthropicModelCompat {
    pub force_adaptive_thinking: bool,
    pub supports_temperature: bool,
    pub thinking_level_map: BTreeMap<String, Value>,
}

fn static_level_map(pairs: &[(&str, Option<&str>)]) -> BTreeMap<String, Value> {
    pairs
        .iter()
        .map(|(level, effort)| {
            let value = match effort {
                Some(s) => Value::String((*s).to_string()),
                None => Value::Null,
            };
            ((*level).to_string(), value)
        })
        .collect()
}

/// Resolve the static metadata for a model ID. The list mirrors the Claude
/// models in the installed pi-ai catalog that set `forceAdaptiveThinking`; the
/// ID is matched without its provider prefix so `anthropic/<id>` works too.
pub fn anthropic_model_compat(model_id: &str) -> AnthropicModelCompat {
    let id = model_id.rsplit('/').next().unwrap_or(model_id);
    // (force_adaptive, supports_temperature, level map)
    let (adaptive, temperature, map): (bool, bool, &[(&str, Option<&str>)]) =
        if id.starts_with("claude-opus-4-6") {
            (true, true, &[("max", Some("max"))])
        } else if id.starts_with("claude-opus-4-7") || id.starts_with("claude-opus-4-8") {
            (
                true,
                false,
                &[("xhigh", Some("xhigh")), ("max", Some("max"))],
            )
        } else if id.starts_with("claude-opus-5-5") {
            (
                true,
                false,
                &[
                    ("off", None),
                    ("minimal", None),
                    ("low", Some("low")),
                    ("medium", Some("medium")),
                    ("high", Some("high")),
                    ("xhigh", Some("xhigh")),
                    ("max", Some("max")),
                ],
            )
        } else if id.starts_with("claude-opus-5") {
            (
                true,
                false,
                &[
                    ("off", None),
                    ("xhigh", Some("xhigh")),
                    ("max", Some("max")),
                ],
            )
        } else if id.starts_with("claude-sonnet-5-5") {
            (
                true,
                false,
                &[
                    ("off", None),
                    ("minimal", None),
                    ("low", Some("low")),
                    ("medium", Some("medium")),
                    ("high", Some("high")),
                    ("xhigh", Some("xhigh")),
                    ("max", Some("max")),
                ],
            )
        } else if id.starts_with("claude-sonnet-5") {
            (
                true,
                true,
                &[("xhigh", Some("xhigh")), ("max", Some("max"))],
            )
        } else if id.starts_with("claude-sonnet-4-6") {
            (true, true, &[("max", Some("max"))])
        } else if id.starts_with("claude-fable-5") {
            (
                true,
                true,
                &[
                    ("off", None),
                    ("xhigh", Some("xhigh")),
                    ("max", Some("max")),
                ],
            )
        } else {
            (false, true, &[])
        };
    AnthropicModelCompat {
        force_adaptive_thinking: adaptive,
        supports_temperature: temperature,
        thinking_level_map: static_level_map(map),
    }
}

/// pi's `mapThinkingLevelToEffort`: an explicit `thinkingLevelMap` string wins,
/// otherwise minimal/low→low, medium→medium, high→high, and anything else
/// (including xhigh/max without a mapping) → high.
fn map_thinking_level_to_effort(map: &BTreeMap<String, Value>, level: &str) -> String {
    if let Some(effort) = mapped_effort(map, level) {
        return effort;
    }
    match level {
        "minimal" | "low" => "low".to_string(),
        "medium" => "medium".to_string(),
        "high" => "high".to_string(),
        _ => "high".to_string(),
    }
}

/// pi's non-adaptive thinking budget math. `model_max` is the model's response
/// ceiling when known (`compat.maxTokens`); without it the request ceiling is
/// used unchanged and only the answer-room clamp applies. Returns
/// `(response_ceiling, thinking_budget_tokens)`.
fn adjust_max_tokens_for_thinking(
    base_max: u32,
    model_max: Option<u32>,
    level: ThinkingLevel,
) -> (u32, u32) {
    let budget = level.budget_tokens();
    let max_tokens = match model_max {
        Some(cap) => base_max.saturating_add(budget).min(cap),
        None => base_max,
    };
    // Anthropic requires budget_tokens >= 1024 AND < max_tokens; when the
    // answer room can't satisfy both, drop thinking entirely instead of
    // emitting an invalid budget.
    let answer_room = max_tokens.saturating_sub(1024);
    let budget = if answer_room >= 1024 {
        budget.min(answer_room)
    } else {
        0
    };
    (max_tokens, budget)
}

/// Decode one Anthropic SSE payload into stream events.
pub fn anthropic_event(payload: &str, out: &mut Vec<StreamEvent>) {
    if payload == "[DONE]" {
        return;
    }
    let v: Value = match serde_json::from_str(payload) {
        Ok(v) => v,
        Err(_) => return,
    };
    let etype = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
    match etype {
        "message_start" => {
            // Anthropic reports input/cache tokens here; output arrives as a
            // CUMULATIVE-FINAL value in `message_delta`. The agent accumulates
            // Usage events as increments, so the 1-2 token seed here must not
            // be added on top of the final value.
            let mut u = parse_usage(v.get("message").and_then(|m| m.get("usage")));
            u.output = 0;
            out.push(StreamEvent::Usage(u));
        }
        "content_block_start" => {
            let idx = v.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as usize;
            if let Some(block) = v.get("content_block") {
                let btype = block.get("type").and_then(|t| t.as_str()).unwrap_or("");
                if btype == "tool_use" {
                    let id = block
                        .get("id")
                        .and_then(|s| s.as_str())
                        .map(str::to_string)
                        .unwrap_or_else(|| gen_id("toolu_"));
                    let name = block
                        .get("name")
                        .and_then(|s| s.as_str())
                        .unwrap_or_default()
                        .to_string();
                    out.push(StreamEvent::ToolCallStart {
                        index: idx,
                        id,
                        name,
                    });
                }
            }
        }
        "content_block_delta" => {
            let idx = v.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as usize;
            if let Some(delta) = v.get("delta") {
                let dtype = delta.get("type").and_then(|t| t.as_str()).unwrap_or("");
                match dtype {
                    "text_delta" => {
                        if let Some(t) = delta.get("text").and_then(|t| t.as_str()) {
                            out.push(StreamEvent::TextDelta(t.to_string()));
                        }
                    }
                    "thinking_delta" => {
                        if let Some(t) = delta.get("thinking").and_then(|t| t.as_str()) {
                            out.push(StreamEvent::ThinkingDelta(t.to_string()));
                        }
                    }
                    "signature_delta" => {
                        if let Some(s) = delta.get("signature").and_then(|s| s.as_str()) {
                            out.push(StreamEvent::ThinkingSignatureDelta(s.to_string()));
                        }
                    }
                    "input_json_delta" => {
                        if let Some(t) = delta.get("partial_json").and_then(|t| t.as_str()) {
                            out.push(StreamEvent::ToolCallDelta {
                                index: idx,
                                json_fragment: t.to_string(),
                            });
                        }
                    }
                    _ => {}
                }
            }
        }
        "message_delta" => {
            let stop = v
                .get("delta")
                .and_then(|d| d.get("stop_reason"))
                .and_then(|s| s.as_str())
                .unwrap_or("end_turn");
            out.push(StreamEvent::Usage(parse_usage(v.get("usage"))));
            out.push(StreamEvent::MessageEnd {
                stop_reason: map_stop(stop),
            });
        }
        "message_stop" | "ping" => {}
        "error" => {
            // mid-stream provider error: surface it instead of truncating
            let msg = v
                .get("error")
                .and_then(|e| e.get("message"))
                .and_then(|m| m.as_str())
                .unwrap_or("unknown provider error");
            out.push(StreamEvent::FatalError(msg.to_string()));
        }
        _ => {}
    }
}

fn parse_usage(u: Option<&Value>) -> Usage {
    let g = |k: &str| {
        u.and_then(|x| x.get(k))
            .and_then(|x| x.as_u64())
            .unwrap_or(0)
    };
    Usage {
        input: g("input_tokens"),
        output: g("output_tokens"),
        cache_read: g("cache_read_input_tokens"),
        cache_write: g("cache_creation_input_tokens"),
    }
}

fn map_stop(s: &str) -> StopReason {
    match s {
        "end_turn" | "stop_sequence" => StopReason::Stop,
        "tool_use" => StopReason::ToolUse,
        "max_tokens" => StopReason::Length,
        // context full: maps to Length so compaction can kick in
        "model_context_window_exceeded" => StopReason::Length,
        // refusal/pause_turn are abnormal terminations — surface as Error
        // instead of silently reporting a clean stop
        "refusal" | "pause_turn" => StopReason::Error,
        _ => StopReason::Stop,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::{Api, ToolCall};

    fn asst_text(text: &str) -> Message {
        Message::assistant(
            vec![ContentBlock::Text { text: text.into() }],
            "m",
            Api::AnthropicMessages,
            Usage::default(),
            StopReason::Stop,
        )
    }

    #[test]
    fn parallel_tool_results_merge_into_one_user_turn() {
        let msgs = vec![
            Message::user_text("go"),
            Message::assistant(
                vec![
                    ContentBlock::ToolCall(ToolCall {
                        id: "1".into(),
                        name: "read".into(),
                        arguments: serde_json::json!({"path":"a"}),
                    }),
                    ContentBlock::ToolCall(ToolCall {
                        id: "2".into(),
                        name: "read".into(),
                        arguments: serde_json::json!({"path":"b"}),
                    }),
                ],
                "m",
                Api::AnthropicMessages,
                Usage::default(),
                StopReason::ToolUse,
            ),
            Message::tool_result("1", "read", "ra", false),
            Message::tool_result("2", "read", "rb", false),
        ];
        let out = AnthropicProvider::convert_messages(&msgs);
        assert_eq!(out.len(), 3, "two tool_results must share one user turn");
        assert_eq!(out[2]["role"], "user");
        let blocks = out[2]["content"].as_array().unwrap();
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0]["type"], "tool_result");
        assert_eq!(blocks[0]["tool_use_id"], "1");
    }

    #[test]
    fn converted_index_tracks_merged_groups() {
        let msgs = vec![
            Message::user_text("u1"),
            asst_text("a1"),
            Message::tool_result("1", "read", "r1", false),
            Message::tool_result("2", "read", "r2", false),
            Message::user_text("u2"),
            asst_text("a2"),
        ];
        assert_eq!(AnthropicProvider::converted_index(&msgs, 0), Some(0));
        assert_eq!(AnthropicProvider::converted_index(&msgs, 1), Some(1));
        assert_eq!(AnthropicProvider::converted_index(&msgs, 2), Some(2));
        assert_eq!(AnthropicProvider::converted_index(&msgs, 3), Some(2));
        // the follow-up user prompt merges into the same turn as the tool
        // results it follows (both are user-side)
        assert_eq!(AnthropicProvider::converted_index(&msgs, 4), Some(2));
        assert_eq!(AnthropicProvider::converted_index(&msgs, 5), Some(3));
        assert_eq!(AnthropicProvider::converted_index(&msgs, 99), None);
    }

    #[test]
    fn usage_decodes_from_message_start_and_delta() {
        let mut events = Vec::new();
        anthropic_event(
            r#"{"type":"message_start","message":{"usage":{"input_tokens":100,"cache_read_input_tokens":40,"cache_creation_input_tokens":10,"output_tokens":1}}}"#,
            &mut events,
        );
        anthropic_event(
            r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":33}}"#,
            &mut events,
        );
        let mut usage = Usage::default();
        let mut stop = None;
        for ev in events {
            match ev {
                StreamEvent::Usage(u) => usage = crate::session::add_usage(usage, u),
                StreamEvent::MessageEnd { stop_reason } => stop = Some(stop_reason),
                _ => {}
            }
        }
        // message_start's output seed is zeroed: message_delta carries the
        // CUMULATIVE-FINAL output (33) and the agent accumulates increments —
        // keeping the seed would double-count it (old behavior said 34)
        assert_eq!(
            (
                usage.input,
                usage.cache_read,
                usage.cache_write,
                usage.output
            ),
            (100, 40, 10, 33)
        );
        assert_eq!(stop, Some(StopReason::ToolUse));
    }

    #[test]
    fn signature_delta_is_captured() {
        let mut events = Vec::new();
        anthropic_event(
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"sigABC"}}"#,
            &mut events,
        );
        assert!(matches!(
            events.first(),
            Some(StreamEvent::ThinkingSignatureDelta(s)) if s == "sigABC"
        ));
    }
}
