//! OpenAI-compatible chat-completions provider (streaming).
//!
//! Works with api.openai.com and any compatible gateway declared in
//! settings (base_url override). OpenAI prefix-caches automatically; the
//! cache-optimizer contributes by keeping request byte-stability, so no
//! explicit markers are sent here.

use super::compat::{
    clamp_level, effective_effort, get_bool, get_str, mapped_effort, parse_level_map,
};
use super::{
    http_post_sse, ChatRequest, EventHandler, ProviderError, ResponseStream, StreamEvent,
    ThinkingLevel,
};
use crate::message::{ContentBlock, Message, Role, StopReason, Usage};
use serde_json::{json, Value};
use std::collections::BTreeMap;

pub const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";

pub struct OpenAiProvider {
    client: reqwest::Client,
    credentials: super::credentials::CredentialSource,
    base_url: String,
    /// Provider/model `compat` metadata handed in by the caller (pi catalog
    /// shape). Empty for the plain constructors; see [`Self::with_compat`].
    compat: Value,
}

impl OpenAiProvider {
    pub fn new(api_key: impl Into<String>, base_url: Option<String>) -> Self {
        Self {
            client: super::shared_client(),
            credentials: super::credentials::CredentialSource::new(api_key),
            base_url: base_url.unwrap_or_else(|| DEFAULT_BASE_URL.to_string()),
            compat: Value::Null,
        }
    }

    /// Attach pi-style compatibility metadata (`compat`). Only the fields
    /// feather actually implements are honored (see [`OpenAiCompat`]); absent
    /// keys keep feather's existing request shape. Chainable so existing
    /// `new(...)` call sites stay source-compatible.
    pub fn with_compat(mut self, compat: Value) -> Self {
        self.compat = compat;
        self
    }

    pub(crate) fn compaction_thinking(
        &self,
        active: Option<ThinkingLevel>,
    ) -> Option<ThinkingLevel> {
        super::compat::compaction_thinking(&self.compat, active)
    }

    pub fn with_credentials(
        mut self,
        resolver: std::sync::Arc<dyn super::credentials::CredentialResolver>,
    ) -> Self {
        self.credentials = self.credentials.with_resolver(resolver);
        self
    }

    pub fn convert_messages(messages: &[Message]) -> Vec<Value> {
        let mut out: Vec<Value> = Vec::new();
        for m in messages {
            match m.role {
                Role::User => {
                    if !m
                        .content
                        .iter()
                        .any(|b| matches!(b, ContentBlock::Image { .. }))
                    {
                        out.push(json!({ "role": "user", "content": m.text() }));
                    } else {
                        let content: Vec<Value> = m
                            .content
                            .iter()
                            .filter_map(|b| match b {
                                ContentBlock::Text { text } if !text.is_empty() => {
                                    Some(json!({ "type": "text", "text": text }))
                                }
                                ContentBlock::Image { media_type, data } => Some(json!({
                                    "type": "image_url",
                                    "image_url": {
                                        "url": format!("data:{media_type};base64,{data}"),
                                    },
                                })),
                                _ => None,
                            })
                            .collect();
                        out.push(json!({ "role": "user", "content": content }));
                    }
                }
                Role::Assistant => {
                    let text = m.text();
                    let calls: Vec<Value> = m
                        .tool_calls()
                        .iter()
                        .map(|c| {
                            json!({
                                "id": c.id,
                                "type": "function",
                                "function": {
                                    "name": c.name,
                                    "arguments": c.arguments.to_string(),
                                },
                            })
                        })
                        .collect();
                    let mut msg = json!({ "role": "assistant" });
                    if !text.is_empty() {
                        msg["content"] = json!(text);
                    } else {
                        // null content is rejected by some strict gateways
                        msg["content"] = json!("");
                    }
                    if !calls.is_empty() {
                        msg["tool_calls"] = json!(calls);
                    }
                    out.push(msg);
                }
                Role::ToolResult => out.push(json!({
                    "role": "tool",
                    "tool_call_id": m.tool_call_id.clone().unwrap_or_default(),
                    "content": m.text(),
                })),
                Role::System => {}
            }
        }
        out
    }

    pub fn build_body(req: &ChatRequest) -> Value {
        Self::build_body_with_compat(req, &Value::Null)
    }

    /// Compat-aware body assembly. `compat` is the caller-supplied pi catalog
    /// object; `Value::Null` keeps feather's existing request shape.
    pub fn build_body_with_compat(req: &ChatRequest, compat: &Value) -> Value {
        let mut body = Self::build_body_base(req, compat);
        // pi-cache-optimizer alignment: per-session cache routing key for
        // OpenAI-compatible providers. One-way hash of the session id (the
        // raw id never leaves the process); never overwrites an existing key.
        if !req.session_id.is_empty() && body.get("prompt_cache_key").is_none() {
            body["prompt_cache_key"] = json!(derive_prompt_cache_key(&req.session_id));
        }
        body
    }

    fn build_body_base(req: &ChatRequest, compat: &Value) -> Value {
        let compat = OpenAiCompat::resolve(compat, req);
        let tools: Vec<Value> = req
            .tools
            .iter()
            .map(|t| {
                json!({
                    "type": "function",
                    "function": {
                        "name": t.name,
                        "description": t.description,
                        "parameters": t.parameters,
                    },
                })
            })
            .collect();
        let mut body = json!({
            "model": req.model,
            "stream": true,
            "stream_options": { "include_usage": true },
            "messages": Self::convert_messages(&req.messages),
            "tools": tools,
        });
        if !req.system.is_empty() {
            body["messages"].as_array_mut().unwrap().insert(
                0,
                json!({ "role": compat.instruction_role(), "content": req.system }),
            );
        }
        if compat.supports_store {
            body["store"] = json!(false);
        }
        match compat.max_tokens_field.as_deref() {
            // Deprecated by OpenAI, but some OpenAI-compatible providers only
            // accept `max_tokens`.
            Some("max_tokens") => body["max_tokens"] = json!(req.max_tokens),
            Some("max_completion_tokens") => body["max_completion_tokens"] = json!(req.max_tokens),
            _ => {}
        }
        // pi sends temperature independently of reasoning on this API; only
        // Anthropic suppresses it for thinking.
        if let Some(t) = req.temperature {
            body["temperature"] = json!(t);
        }
        apply_openai_thinking(&mut body, &compat, req.thinking);
        body
    }

    pub async fn stream(&self, req: &ChatRequest) -> Result<ResponseStream, ProviderError> {
        let cred = self.credentials.resolve().await?;
        let base = cred.base_url.as_deref().unwrap_or(&self.base_url);
        let url = format!("{}/chat/completions", base.trim_end_matches('/'));
        let mut headers = cred.request_headers(req);
        headers.push(("authorization".into(), format!("Bearer {}", cred.api_key)));
        // Some gateways report usage in the finish chunk, some in a trailing
        // usage-only chunk, a few in both (full values, which would double
        // count when accumulated). Keep the first usage report per response.
        let mut usage_seen = false;
        let handler: EventHandler = Box::new(move |payload, out| {
            let start = out.len();
            openai_event(payload, out);
            let mut i = start;
            while i < out.len() {
                if matches!(out[i], StreamEvent::Usage(_)) {
                    if usage_seen {
                        out.remove(i);
                        continue;
                    }
                    usage_seen = true;
                }
                i += 1;
            }
        });
        http_post_sse(
            &self.client,
            &url,
            headers,
            Self::build_body_with_compat(req, &self.compat),
            handler,
        )
        .await
    }
}

/// Resolved OpenAI-compatible `compat`, limited to the fields feather
/// implements. `supportsStore`, `supportsDeveloperRole` and `maxTokensField`
/// are opt-in (absent = feather's existing request shape);
/// `supportsReasoningEffort` defaults on, matching the previous unconditional
/// `reasoning_effort`.
#[derive(Debug, Clone)]
pub struct OpenAiCompat {
    pub supports_store: bool,
    pub supports_developer_role: bool,
    pub supports_reasoning_effort: bool,
    pub max_tokens_field: Option<String>,
    pub thinking_format: String,
    pub thinking_level_map: BTreeMap<String, Value>,
    /// True when the caller supplied an explicit `thinkingLevelMap`; only then
    /// is pi's level clamping applied (an absent map keeps feather's direct
    /// minimal..max passthrough).
    pub has_level_map: bool,
    pub reasoning: bool,
}

impl OpenAiCompat {
    fn resolve(compat: &Value, req: &ChatRequest) -> Self {
        let level_map = parse_level_map(compat);
        Self {
            supports_store: get_bool(compat, "supportsStore").unwrap_or(false),
            supports_developer_role: get_bool(compat, "supportsDeveloperRole").unwrap_or(false),
            supports_reasoning_effort: get_bool(compat, "supportsReasoningEffort").unwrap_or(true),
            max_tokens_field: get_str(compat, "maxTokensField").map(str::to_string),
            thinking_format: get_str(compat, "thinkingFormat")
                .unwrap_or("openai")
                .to_string(),
            has_level_map: level_map.is_some(),
            thinking_level_map: level_map.unwrap_or_default(),
            // feather has no `reasoning` flag on ChatRequest; an explicit compat
            // value wins, otherwise a requested thinking level implies a
            // reasoning model (explicit `off` still lets the map's off value
            // through, like pi).
            reasoning: get_bool(compat, "reasoning").unwrap_or(req.thinking.is_some()),
        }
    }

    fn instruction_role(&self) -> &'static str {
        if self.reasoning && self.supports_developer_role {
            "developer"
        } else {
            "system"
        }
    }
}

/// Apply pi's `thinkingFormat` handling. Only the formats feather implements
/// are special-cased; an unknown format falls back to OpenAI-style
/// `reasoning_effort`.
fn apply_openai_thinking(body: &mut Value, compat: &OpenAiCompat, thinking: Option<ThinkingLevel>) {
    if !compat.reasoning {
        return;
    }
    let raw = thinking.map(|l| l.as_str()).unwrap_or("off");
    // pi clamps the requested level against the model map before mapping it.
    // feather only clamps when the caller supplied a custom map, so an absent
    // map keeps the existing minimal..max passthrough.
    let level = if compat.has_level_map {
        clamp_level(&compat.thinking_level_map, raw)
    } else {
        raw.to_string()
    };
    let effort: Option<&str> = if level == "off" {
        None
    } else {
        Some(level.as_str())
    };
    let map = &compat.thinking_level_map;
    let off_not_null = !matches!(map.get("off"), Some(Value::Null));
    let off_value = |dflt: &str| match map.get("off") {
        Some(Value::String(s)) => s.clone(),
        _ => dflt.to_string(),
    };
    match compat.thinking_format.as_str() {
        "zai" => {
            body["thinking"] = if effort.is_some() {
                json!({ "type": "enabled", "clear_thinking": false })
            } else {
                json!({ "type": "disabled" })
            };
            if let Some(level) = effort {
                if compat.supports_reasoning_effort {
                    body["reasoning_effort"] = json!(effective_effort(map, level));
                }
            }
        }
        "qwen" => {
            body["enable_thinking"] = json!(effort.is_some());
            if let Some(level) = effort {
                if compat.supports_reasoning_effort {
                    body["reasoning_effort"] = json!(effective_effort(map, level));
                }
            }
        }
        "qwen-chat-template" => {
            body["chat_template_kwargs"] =
                json!({ "enable_thinking": effort.is_some(), "preserve_thinking": true });
        }
        "deepseek" => {
            if effort.is_some() {
                body["thinking"] = json!({ "type": "enabled" });
            } else if off_not_null {
                body["thinking"] = json!({ "type": "disabled" });
            }
            if let Some(level) = effort {
                if compat.supports_reasoning_effort {
                    body["reasoning_effort"] = json!(effective_effort(map, level));
                }
            }
        }
        "openrouter" => {
            if let Some(level) = effort {
                body["reasoning"] = json!({ "effort": effective_effort(map, level) });
            } else if off_not_null {
                body["reasoning"] = json!({ "effort": off_value("none") });
            }
        }
        "ant-ling" => {
            if let Some(level) = effort {
                if let Some(mapped) = mapped_effort(map, level) {
                    body["reasoning"] = json!({ "effort": mapped });
                }
            }
        }
        "together" => {
            body["reasoning"] = json!({ "enabled": effort.is_some() });
            if let Some(level) = effort {
                if compat.supports_reasoning_effort {
                    body["reasoning_effort"] = json!(effective_effort(map, level));
                }
            }
        }
        "string-thinking" => {
            if let Some(level) = effort {
                body["thinking"] = json!(effective_effort(map, level));
            } else if off_not_null {
                body["thinking"] = json!(off_value("none"));
            }
        }
        // OpenAI-style `reasoning_effort` (default; also the fallback for
        // formats feather does not special-case, e.g. chat-template/baseten).
        _ => {
            if let Some(level) = effort {
                if compat.supports_reasoning_effort {
                    body["reasoning_effort"] = json!(effective_effort(map, level));
                }
            } else if compat.supports_reasoning_effort {
                if let Some(Value::String(s)) = map.get("off") {
                    body["reasoning_effort"] = json!(s);
                }
            }
        }
    }
}

/// pi-cache-optimizer's derivation: `pi-<sha256(session_id)[0..32 hex]>`.
/// Sticky per-session routing without exposing the raw session id.
pub fn derive_prompt_cache_key(session_id: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(session_id.as_bytes());
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    format!("pi-{}", &hex[..32])
}

/// Decode one OpenAI SSE payload. Tool-call arguments arrive fragmented
/// across `delta.tool_calls[i].function.arguments`.
/// across `delta.tool_calls[i].function.arguments`.
pub fn openai_event(payload: &str, out: &mut Vec<StreamEvent>) {
    if payload == "[DONE]" {
        return;
    }
    let v: Value = match serde_json::from_str(payload) {
        Ok(v) => v,
        Err(_) => return,
    };
    if let Some(err) = v.get("error") {
        // a hard provider error, not assistant content: surface as failure
        let msg = err
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("unknown error");
        out.push(StreamEvent::FatalError(msg.to_string()));
        return;
    }
    let choices = match v.get("choices").and_then(|c| c.as_array()) {
        Some(c) => c,
        None => return,
    };
    if let Some(choice) = choices.first() {
        if let Some(delta) = choice.get("delta") {
            if let Some(text) = delta.get("content").and_then(|c| c.as_str()) {
                if !text.is_empty() {
                    out.push(StreamEvent::TextDelta(text.to_string()));
                }
            }
            // reasoning models (DeepSeek / OpenRouter style) stream thinking
            // via `reasoning_content` or `reasoning` instead of `content`
            for key in ["reasoning_content", "reasoning"] {
                if let Some(t) = delta.get(key).and_then(|c| c.as_str()) {
                    if !t.is_empty() {
                        out.push(StreamEvent::ThinkingDelta(t.to_string()));
                    }
                }
            }
            if let Some(calls) = delta.get("tool_calls").and_then(|c| c.as_array()) {
                for c in calls {
                    let index = c.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as usize;
                    if let Some(id) = c.get("id").and_then(|i| i.as_str()) {
                        let name = c
                            .get("function")
                            .and_then(|f| f.get("name"))
                            .and_then(|n| n.as_str())
                            .unwrap_or_default();
                        out.push(StreamEvent::ToolCallStart {
                            index,
                            id: id.to_string(),
                            name: name.to_string(),
                        });
                    }
                    if let Some(args) = c
                        .get("function")
                        .and_then(|f| f.get("arguments"))
                        .and_then(|a| a.as_str())
                    {
                        if !args.is_empty() {
                            out.push(StreamEvent::ToolCallDelta {
                                index,
                                json_fragment: args.to_string(),
                            });
                        }
                    }
                }
            }
        }
        if let Some(reason) = choice.get("finish_reason").and_then(|r| r.as_str()) {
            // some gateways include the usage in the finish chunk, others in a
            // final usage-only chunk; both arrive as Usage increments
            if let Some(usage) = parse_usage_opt(&v) {
                out.push(StreamEvent::Usage(usage));
            }
            out.push(StreamEvent::MessageEnd {
                stop_reason: map_stop(reason),
            });
        }
    } else if let Some(usage) = parse_usage_opt(&v) {
        // final usage-only chunk
        out.push(StreamEvent::Usage(usage));
    }
}

fn parse_usage_opt(v: &Value) -> Option<Usage> {
    let u = v.get("usage")?;
    if u.is_null() {
        return None;
    }
    let g = |k: &str| u.get(k).and_then(|x| x.as_u64()).unwrap_or(0);
    // prompt_tokens INCLUDES cached tokens; split them out so Usage::total()
    // (input + cache_read + …) matches the Anthropic accounting model
    let cached = u
        .get("prompt_tokens_details")
        .and_then(|d| d.get("cached_tokens"))
        .and_then(|x| x.as_u64())
        .unwrap_or(0);
    let prompt = g("prompt_tokens");
    Some(Usage {
        input: prompt.saturating_sub(cached),
        output: g("completion_tokens"),
        cache_read: cached,
        cache_write: 0,
    })
}

fn map_stop(s: &str) -> StopReason {
    match s {
        "stop" => StopReason::Stop,
        "tool_calls" | "function_call" => StopReason::ToolUse,
        "length" => StopReason::Length,
        // content_filter is an abnormal termination, not a clean stop
        "content_filter" => StopReason::Error,
        _ => StopReason::Stop,
    }
}

// silence unused-variant lint when only constructed via Message::assistant
#[allow(dead_code)]
fn _never(c: &ContentBlock) -> bool {
    matches!(c, ContentBlock::Thinking { .. })
}

#[cfg(test)]
mod cache_key_tests {
    use super::*;
    use crate::provider::CacheHints;

    fn req(session_id: &str) -> ChatRequest {
        ChatRequest {
            system: "s".into(),
            messages: vec![Message::user_text("hi")].into(),
            tools: vec![].into(),
            model: "m".into(),
            max_tokens: 10,
            temperature: None,
            cache: CacheHints::default(),
            thinking: None,
            session_id: session_id.into(),
        }
    }

    #[test]
    fn prompt_cache_key_is_stable_per_session_and_never_exposes_the_id() {
        let a = derive_prompt_cache_key("sess-abc");
        assert_eq!(a, derive_prompt_cache_key("sess-abc"), "sticky per session");
        assert_ne!(a, derive_prompt_cache_key("sess-other"));
        let hex = a.strip_prefix("pi-").unwrap();
        assert_eq!(hex.len(), 32);
        assert!(!a.contains("sess-abc"), "one-way hash only");
    }

    #[test]
    fn build_body_injects_prompt_cache_key_when_session_id_present() {
        let body = OpenAiProvider::build_body(&req("sess-abc"));
        assert_eq!(
            body["prompt_cache_key"],
            json!(derive_prompt_cache_key("sess-abc"))
        );
        // no session id → no key (the field is omitted entirely)
        let body = OpenAiProvider::build_body(&req(""));
        assert!(body.get("prompt_cache_key").is_none());
    }
}
