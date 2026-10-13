//! OpenAI Responses and ChatGPT Codex Responses provider (streaming SSE).
//!
//! Minimal runtime parity for OAuth-backed Codex dialogue: request shaping,
//! tool-call replay, reasoning encrypted-content replay, and streaming deltas.

use super::{http_post_sse, ChatRequest, EventHandler, ProviderError, ResponseStream, StreamEvent};
use crate::message::{Api, ContentBlock, Message, Role, StopReason, Usage};
use crate::provider::compat::{clamp_level, effective_effort, parse_level_map};
use crate::provider::credentials::{CredentialResolver, CredentialSource};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

pub const DEFAULT_RESPONSES_BASE_URL: &str = "https://api.openai.com/v1";
pub const DEFAULT_CODEX_BASE_URL: &str = "https://chatgpt.com/backend-api";

pub struct ResponsesProvider {
    client: reqwest::Client,
    credentials: CredentialSource,
    base_url: Option<String>,
    compat: Value,
    codex: bool,
}

impl ResponsesProvider {
    pub fn new(api_key: impl Into<String>, base_url: Option<String>) -> Self {
        Self {
            client: super::shared_client(),
            credentials: CredentialSource::new(api_key),
            base_url,
            compat: Value::Null,
            codex: false,
        }
    }

    pub fn with_compat(mut self, compat: Value) -> Self {
        self.compat = compat;
        self
    }

    pub(crate) fn compaction_thinking(
        &self,
        active: Option<super::ThinkingLevel>,
    ) -> Option<super::ThinkingLevel> {
        super::compat::compaction_thinking(&self.compat, active)
    }

    pub fn with_credentials(mut self, resolver: Arc<dyn CredentialResolver>) -> Self {
        self.credentials = self.credentials.with_resolver(resolver);
        self
    }

    pub fn api(&self) -> Api {
        if self.codex {
            Api::OpenaiCodexResponses
        } else {
            Api::OpenaiResponses
        }
    }
    pub fn with_codex(mut self, codex: bool) -> Self {
        self.codex = codex;
        self
    }

    pub async fn stream(&self, req: &ChatRequest) -> Result<ResponseStream, ProviderError> {
        let cred = self.credentials.resolve().await?;
        let base = cred.base_url.clone().or_else(|| self.base_url.clone());
        let url = if self.codex {
            codex_url(base)
        } else {
            responses_url(base)
        };
        let body = if self.codex {
            build_codex_body(req, &self.compat)
        } else {
            build_responses_body(req, &self.compat)
        };
        let mut headers = cred.request_headers(req);
        headers.push(("authorization".into(), format!("Bearer {}", cred.api_key)));
        if self.codex {
            let account_id = cred.account_id.ok_or_else(|| {
                ProviderError::Auth("OpenAI Codex OAuth account id missing".into())
            })?;
            headers.push(("chatgpt-account-id".into(), account_id));
            headers.push(("originator".into(), "pi".into()));
            headers.push(("OpenAI-Beta".into(), "responses=experimental".into()));
            if !req.session_id.is_empty() {
                headers.push(("session-id".into(), req.session_id.clone()));
                headers.push(("x-client-request-id".into(), req.session_id.clone()));
            }
        }
        headers.push(("accept".into(), "text/event-stream".into()));
        headers.push(("content-type".into(), "application/json".into()));
        let mut state = ResponsesEventState::default();
        let handler: EventHandler =
            Box::new(move |payload, out| responses_event(payload, out, &mut state));
        http_post_sse(&self.client, &url, headers, body, handler).await
    }
}

fn responses_url(base: Option<String>) -> String {
    format!(
        "{}/responses",
        base.unwrap_or_else(|| DEFAULT_RESPONSES_BASE_URL.into())
            .trim_end_matches('/')
    )
}

fn codex_url(base: Option<String>) -> String {
    let n = base
        .unwrap_or_else(|| DEFAULT_CODEX_BASE_URL.into())
        .trim_end_matches('/')
        .to_string();
    if n.ends_with("/codex/responses") {
        n
    } else if n.ends_with("/codex") {
        format!("{n}/responses")
    } else {
        format!("{n}/codex/responses")
    }
}

pub fn build_responses_body(req: &ChatRequest, compat: &Value) -> Value {
    let mut body = json!({
        "model": req.model,
        "input": convert_messages(req, true, Api::OpenaiResponses),
        "stream": true,
        "store": false,
        "max_output_tokens": req.max_tokens.max(16),
    });
    if !req.session_id.is_empty() {
        body["prompt_cache_key"] = json!(crate::provider::openai::derive_prompt_cache_key(
            &req.session_id
        ));
    }
    if let Some(t) = req.temperature {
        body["temperature"] = json!(t);
    }
    add_tools_and_reasoning(&mut body, req, compat, false);
    body
}

pub fn build_codex_body(req: &ChatRequest, compat: &Value) -> Value {
    let mut body = json!({
        "model": req.model,
        "store": false,
        "stream": true,
        "instructions": if req.system.is_empty() { "You are a helpful assistant." } else { &req.system },
        "input": convert_messages(req, false, Api::OpenaiCodexResponses),
        "text": { "verbosity": "low" },
        "include": ["reasoning.encrypted_content"],
        "tool_choice": "auto",
        "parallel_tool_calls": true,
    });
    if !req.session_id.is_empty() {
        body["prompt_cache_key"] = json!(crate::provider::openai::derive_prompt_cache_key(
            &req.session_id
        ));
    }
    add_tools_and_reasoning(&mut body, req, compat, true);
    body
}

fn add_tools_and_reasoning(body: &mut Value, req: &ChatRequest, compat: &Value, codex: bool) {
    if !req.tools.is_empty() {
        body["tools"] = json!(req.tools.iter().map(|t| json!({"type":"function","name":t.name,"description":t.description,"parameters":t.parameters,"strict": if codex { Value::Null } else { Value::Bool(false) }})).collect::<Vec<_>>());
    }
    if let Some(level) = req.thinking {
        if level != super::ThinkingLevel::Off {
            let raw = level.as_str();
            let map = parse_level_map(compat).unwrap_or_default();
            let clamped = if map.is_empty() {
                raw.to_string()
            } else {
                clamp_level(&map, raw)
            };
            let mapped = if map.is_empty() {
                json!(clamped)
            } else {
                json!(effective_effort(&map, &clamped))
            };
            if !mapped.is_null() {
                body["reasoning"] = json!({"effort": mapped, "summary": "auto"});
                if !codex {
                    body["include"] = json!(["reasoning.encrypted_content"]);
                }
            }
        }
    }
}

fn convert_messages(req: &ChatRequest, include_system: bool, api: Api) -> Vec<Value> {
    let mut out = Vec::new();
    if include_system && !req.system.is_empty() {
        out.push(json!({"role":"developer","content":req.system}));
    }
    for m in req.messages.iter() {
        match m.role {
            Role::User => out.push(json!({"role":"user","content":user_content(m)})),
            Role::Assistant => convert_assistant(m, &mut out, api, &req.model),
            Role::ToolResult => out.push(json!({"type":"function_call_output","call_id":m.tool_call_id.clone().unwrap_or_default().split('|').next().unwrap_or_default(),"output":m.text()})),
            Role::System => if include_system { out.push(json!({"role":"developer","content":m.text()})); },
        }
    }
    out
}

fn user_content(m: &Message) -> Vec<Value> {
    let mut content = Vec::new();
    for b in &m.content {
        match b {
            ContentBlock::Text { text } if !text.is_empty() => content.push(json!({"type":"input_text","text":text})),
            ContentBlock::Image { media_type, data } => content.push(json!({"type":"input_image","detail":"auto","image_url":format!("data:{media_type};base64,{data}")})),
            _ => {}
        }
    }
    content
}

fn convert_assistant(m: &Message, out: &mut Vec<Value>, api: Api, model: &str) {
    let same_api_model = m.api == Some(api) && m.model.as_deref() == Some(model);
    for b in &m.content {
        match b {
            ContentBlock::Text { text } if !text.is_empty() => out.push(json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":text,"annotations":[]}],"status":"completed"})),
            ContentBlock::Thinking { text, signature } => {
                let replay = signature
                    .as_deref()
                    .and_then(|sig| serde_json::from_str::<Value>(sig).ok())
                    .filter(|v| same_api_model && v.get("type").and_then(|t| t.as_str()) == Some("reasoning") && v.get("encrypted_content").is_some());
                if let Some(v) = replay {
                    out.push(v);
                } else if !text.is_empty() {
                    out.push(json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":format!("<thinking>\n{}\n</thinking>", text),"annotations":[]}],"status":"completed"}));
                }
            }
            ContentBlock::ToolCall(c) => {
                let mut parts = c.id.split('|');
                let call_id = parts.next().unwrap_or(&c.id);
                let id = parts.next();
                let mut v = json!({"type":"function_call","call_id":call_id,"name":c.name,"arguments":c.arguments.to_string()});
                if let Some(id) = id { v["id"] = json!(id); }
                out.push(v);
            }
            _ => {}
        }
    }
}

#[derive(Default)]
pub struct ResponsesEventState {
    tool_started: HashSet<usize>,
    tool_args: HashMap<usize, String>,
    text: HashMap<usize, String>,
    thinking: HashMap<usize, String>,
    tools_emitted: bool,
    terminal: bool,
}

pub fn responses_event(payload: &str, out: &mut Vec<StreamEvent>, state: &mut ResponsesEventState) {
    if payload == "[DONE]" {
        return;
    }
    let v: Value = match serde_json::from_str(payload) {
        Ok(v) => v,
        Err(_) => return,
    };
    if let Some(err) = v.get("error") {
        out.push(StreamEvent::FatalError(
            err.get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("unknown error")
                .into(),
        ));
        return;
    }
    match v.get("type").and_then(|t| t.as_str()).unwrap_or("") {
        "response.output_item.added" => {
            if let (Some(i), Some(item)) = (
                v.get("output_index").and_then(|i| i.as_u64()),
                v.get("item"),
            ) {
                start_item(i as usize, item, out, state);
            }
        }
        "response.output_item.done" => {
            if let (Some(i), Some(item)) = (
                v.get("output_index").and_then(|i| i.as_u64()),
                v.get("item"),
            ) {
                done_item(i as usize, item, out, state);
            }
        }
        "response.output_text.delta" | "response.refusal.delta" => {
            if let (Some(i), Some(d)) = (
                v.get("output_index").and_then(|i| i.as_u64()),
                v.get("delta").and_then(|d| d.as_str()),
            ) {
                state.text.entry(i as usize).or_default().push_str(d);
                out.push(StreamEvent::TextDelta(d.into()));
            }
        }
        "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
            if let (Some(i), Some(d)) = (
                v.get("output_index").and_then(|i| i.as_u64()),
                v.get("delta").and_then(|d| d.as_str()),
            ) {
                state.thinking.entry(i as usize).or_default().push_str(d);
                out.push(StreamEvent::ThinkingDelta(d.into()));
            }
        }
        "response.function_call_arguments.delta" => {
            if let (Some(i), Some(d)) = (
                v.get("output_index").and_then(|i| i.as_u64()),
                v.get("delta").and_then(|d| d.as_str()),
            ) {
                let i = i as usize;
                state.tool_args.entry(i).or_default().push_str(d);
                out.push(StreamEvent::ToolCallDelta {
                    index: i,
                    json_fragment: d.into(),
                });
            }
        }
        "response.function_call_arguments.done" => {
            if let (Some(i), Some(a)) = (
                v.get("output_index").and_then(|i| i.as_u64()),
                v.get("arguments").and_then(|a| a.as_str()),
            ) {
                let i = i as usize;
                let prior = state.tool_args.entry(i).or_default();
                if let Some(rest) = a.strip_prefix(prior.as_str()) {
                    if !rest.is_empty() {
                        out.push(StreamEvent::ToolCallDelta {
                            index: i,
                            json_fragment: rest.into(),
                        });
                    }
                }
                *prior = a.to_string();
            }
        }
        "response.completed" | "response.incomplete" => {
            finalize(v.get("response").unwrap_or(&v), out, state)
        }
        "response.failed" => out.push(StreamEvent::FatalError(
            v.pointer("/response/error/message")
                .and_then(|m| m.as_str())
                .unwrap_or("response failed")
                .into(),
        )),
        "error" => out.push(StreamEvent::FatalError(
            v.get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("unknown error")
                .into(),
        )),
        _ => {}
    }
}

fn start_item(
    index: usize,
    item: &Value,
    out: &mut Vec<StreamEvent>,
    state: &mut ResponsesEventState,
) {
    if item.get("type").and_then(|t| t.as_str()) == Some("function_call") {
        state.tool_started.insert(index);
        state.tools_emitted = true;
        let call_id = item
            .get("call_id")
            .and_then(|s| s.as_str())
            .unwrap_or_default();
        let id = item.get("id").and_then(|s| s.as_str());
        let stored_id = id
            .map(|id| format!("{call_id}|{id}"))
            .unwrap_or_else(|| call_id.to_string());
        out.push(StreamEvent::ToolCallStart {
            index,
            id: stored_id,
            name: item
                .get("name")
                .and_then(|s| s.as_str())
                .unwrap_or_default()
                .to_string(),
        });
        if let Some(a) = item
            .get("arguments")
            .and_then(|a| a.as_str())
            .filter(|s| !s.is_empty())
        {
            state.tool_args.insert(index, a.to_string());
            out.push(StreamEvent::ToolCallDelta {
                index,
                json_fragment: a.into(),
            });
        }
    }
}

fn done_item(
    index: usize,
    item: &Value,
    out: &mut Vec<StreamEvent>,
    state: &mut ResponsesEventState,
) {
    match item.get("type").and_then(|t| t.as_str()) {
        Some("function_call") => {
            if !state.tool_started.contains(&index) {
                start_item(index, item, out, state);
            }
            if let Some(a) = item.get("arguments").and_then(|a| a.as_str()) {
                let prior = state.tool_args.entry(index).or_default();
                if let Some(rest) = a.strip_prefix(prior.as_str()) {
                    if !rest.is_empty() {
                        out.push(StreamEvent::ToolCallDelta {
                            index,
                            json_fragment: rest.into(),
                        });
                    }
                }
                *prior = a.to_string();
            }
        }
        Some("reasoning") => {
            let text = item
                .get("summary")
                .and_then(|s| s.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.get("text").and_then(|t| t.as_str()))
                        .collect::<Vec<_>>()
                        .join("\n\n")
                })
                .unwrap_or_default();
            let prior = state.thinking.entry(index).or_default();
            if let Some(rest) = text.strip_prefix(prior.as_str()) {
                if !rest.is_empty() {
                    out.push(StreamEvent::ThinkingDelta(rest.into()));
                }
            } else if prior.is_empty() && !text.is_empty() {
                out.push(StreamEvent::ThinkingDelta(text.clone()));
            }
            *prior = text;
            if item.get("encrypted_content").is_some() {
                out.push(StreamEvent::ThinkingSignatureDelta(item.to_string()));
            }
        }
        Some("message") => {
            if let Some(arr) = item.get("content").and_then(|c| c.as_array()) {
                let text = arr
                    .iter()
                    .filter_map(|c| {
                        c.get("text")
                            .or_else(|| c.get("refusal"))
                            .and_then(|t| t.as_str())
                    })
                    .collect::<String>();
                let prior = state.text.entry(index).or_default();
                if let Some(rest) = text.strip_prefix(prior.as_str()) {
                    if !rest.is_empty() {
                        out.push(StreamEvent::TextDelta(rest.into()));
                    }
                } else if prior.is_empty() && !text.is_empty() {
                    out.push(StreamEvent::TextDelta(text.clone()));
                }
                *prior = text;
            }
        }
        _ => {}
    }
}

fn finalize(resp: &Value, out: &mut Vec<StreamEvent>, state: &mut ResponsesEventState) {
    if state.terminal {
        return;
    }
    state.terminal = true;
    if let Some(u) = resp.get("usage") {
        out.push(StreamEvent::Usage(parse_usage(u)));
    }
    out.push(StreamEvent::MessageEnd {
        stop_reason: map_status(resp, state.tools_emitted),
    });
}

fn parse_usage(u: &Value) -> Usage {
    let g = |k: &str| u.get(k).and_then(|x| x.as_u64()).unwrap_or(0);
    let details = u.get("input_tokens_details");
    let cached = details
        .and_then(|d| d.get("cached_tokens"))
        .and_then(|x| x.as_u64())
        .unwrap_or(0);
    let written = details
        .and_then(|d| d.get("cache_write_tokens"))
        .and_then(|x| x.as_u64())
        .unwrap_or(0);
    Usage {
        input: g("input_tokens").saturating_sub(cached + written),
        output: g("output_tokens"),
        cache_read: cached,
        cache_write: written,
    }
}
fn map_status(resp: &Value, tools_emitted: bool) -> StopReason {
    if tools_emitted
        && resp
            .get("status")
            .and_then(|s| s.as_str())
            .unwrap_or("completed")
            == "completed"
    {
        return StopReason::ToolUse;
    }
    match resp
        .get("status")
        .and_then(|s| s.as_str())
        .unwrap_or("completed")
    {
        "completed" => StopReason::Stop,
        "incomplete" => {
            if resp
                .pointer("/incomplete_details/reason")
                .and_then(|r| r.as_str())
                == Some("max_output_tokens")
            {
                StopReason::Length
            } else {
                StopReason::Error
            }
        }
        "cancelled" => StopReason::Aborted,
        "failed" => StopReason::Error,
        _ => StopReason::Stop,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{CacheHints, ThinkingLevel};
    fn req() -> ChatRequest {
        ChatRequest {
            system: "sys".into(),
            messages: vec![Message::user_text("hi")].into(),
            tools: vec![].into(),
            model: "gpt".into(),
            max_tokens: 8,
            temperature: None,
            cache: CacheHints::default(),
            thinking: Some(ThinkingLevel::Low),
            session_id: "sess".into(),
        }
    }
    #[test]
    fn codex_body_shape() {
        let mut r = req();
        r.temperature = Some(0.2);
        let b = build_codex_body(&r, &Value::Null);
        assert_eq!(b["store"], false);
        assert_eq!(b["instructions"], "sys");
        assert_eq!(b["text"]["verbosity"], "low");
        assert!(b.get("temperature").is_none());
        assert!(b["include"]
            .as_array()
            .unwrap()
            .contains(&json!("reasoning.encrypted_content")));
    }
    #[test]
    fn sse_tool_delta_done_not_duplicated() {
        let mut out = vec![];
        let mut st = ResponsesEventState::default();
        responses_event(
            r#"{"type":"response.output_item.added","output_index":0,"item":{"type":"function_call","call_id":"c","id":"fc","name":"t","arguments":""}}"#,
            &mut out,
            &mut st,
        );
        responses_event(
            r#"{"type":"response.function_call_arguments.delta","output_index":0,"delta":"{\"a\":"}"#,
            &mut out,
            &mut st,
        );
        responses_event(
            r#"{"type":"response.function_call_arguments.done","output_index":0,"arguments":"{\"a\":1}"}"#,
            &mut out,
            &mut st,
        );
        assert!(matches!(out[0], StreamEvent::ToolCallStart { .. }));
        assert_eq!(
            out.iter()
                .filter(|e| matches!(e, StreamEvent::ToolCallStart { .. }))
                .count(),
            1
        );
    }

    #[test]
    fn text_and_thinking_done_emit_exactly_once() {
        let mut out = vec![];
        let mut st = ResponsesEventState::default();
        responses_event(
            r#"{"type":"response.output_text.delta","output_index":0,"delta":"hel"}"#,
            &mut out,
            &mut st,
        );
        responses_event(
            r#"{"type":"response.output_item.done","output_index":0,"item":{"type":"message","content":[{"type":"output_text","text":"hello"}]}}"#,
            &mut out,
            &mut st,
        );
        responses_event(
            r#"{"type":"response.reasoning_summary_text.delta","output_index":1,"delta":"th"}"#,
            &mut out,
            &mut st,
        );
        responses_event(
            r#"{"type":"response.output_item.done","output_index":1,"item":{"type":"reasoning","id":"rs","summary":[{"text":"think"}],"encrypted_content":"enc"}}"#,
            &mut out,
            &mut st,
        );
        let text: String = out
            .iter()
            .filter_map(|e| {
                if let StreamEvent::TextDelta(s) = e {
                    Some(s.as_str())
                } else {
                    None
                }
            })
            .collect();
        let thinking: String = out
            .iter()
            .filter_map(|e| {
                if let StreamEvent::ThinkingDelta(s) = e {
                    Some(s.as_str())
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(text, "hello");
        assert_eq!(thinking, "think");
        assert_eq!(
            out.iter()
                .filter(|e| matches!(e, StreamEvent::ThinkingSignatureDelta(_)))
                .count(),
            1
        );
    }

    #[test]
    fn tool_done_suffix_and_completed_maps_tool_use() {
        let mut out = vec![];
        let mut st = ResponsesEventState::default();
        responses_event(
            r#"{"type":"response.output_item.added","output_index":0,"item":{"type":"function_call","call_id":"call","id":"fc_item","name":"tool","arguments":""}}"#,
            &mut out,
            &mut st,
        );
        responses_event(
            r#"{"type":"response.function_call_arguments.delta","output_index":0,"delta":"{\"a\":"}"#,
            &mut out,
            &mut st,
        );
        responses_event(
            r#"{"type":"response.output_item.done","output_index":0,"item":{"type":"function_call","call_id":"call","id":"fc_item","name":"tool","arguments":"{\"a\":1}"}}"#,
            &mut out,
            &mut st,
        );
        responses_event(
            r#"{"type":"response.completed","response":{"status":"completed"}}"#,
            &mut out,
            &mut st,
        );
        let args: String = out
            .iter()
            .filter_map(|e| {
                if let StreamEvent::ToolCallDelta { json_fragment, .. } = e {
                    Some(json_fragment.as_str())
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(args, "{\"a\":1}");
        assert!(matches!(out[0], StreamEvent::ToolCallStart{ ref id, .. } if id == "call|fc_item"));
        assert!(matches!(
            out.last().unwrap(),
            StreamEvent::MessageEnd {
                stop_reason: StopReason::ToolUse
            }
        ));
    }

    #[test]
    fn two_turn_request_pairs_tool_output_and_user_image() {
        let assistant = Message::assistant(
            vec![ContentBlock::ToolCall(crate::message::ToolCall {
                id: "call|fc_item".into(),
                name: "tool".into(),
                arguments: json!({"a":1}),
            })],
            "gpt",
            Api::OpenaiResponses,
            Usage::default(),
            StopReason::ToolUse,
        );
        let tool = Message::tool_result("call|fc_item", "tool", "ok", false);
        let mut user = Message::user_text("see");
        user.content.push(ContentBlock::Image {
            media_type: "image/png".into(),
            data: "AAA".into(),
        });
        let mut r = req();
        r.messages = vec![user, assistant, tool].into();
        let b = build_responses_body(&r, &Value::Null);
        let input = b["input"].as_array().unwrap();
        assert!(input
            .iter()
            .any(|v| v["type"] == "function_call" && v["id"] == "fc_item"));
        assert!(input
            .iter()
            .any(|v| v["type"] == "function_call_output" && v["call_id"] == "call"));
        assert!(input[1]["content"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["type"] == "input_image"));
    }

    struct StaticResolver {
        base_url: String,
    }
    impl crate::provider::credentials::CredentialResolver for StaticResolver {
        fn resolve(&self) -> crate::provider::credentials::CredentialFuture<'_> {
            Box::pin(async move {
                Ok(crate::provider::credentials::ResolvedCredential {
                    api_key: "tok".into(),
                    account_id: Some("acct".into()),
                    base_url: Some(self.base_url.clone()),
                    headers: vec![("x-extra".into(), "yes".into())],
                    ..Default::default()
                })
            })
        }
    }

    #[tokio::test]
    async fn codex_http_sends_url_headers_body_and_streams_tool_history() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::sync::mpsc;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut buf = [0u8; 8192];
            let n = s.read(&mut buf).unwrap();
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            tx.send(req).unwrap();
            let body = concat!(
                "data: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"type\":\"function_call\",\"call_id\":\"c\",\"id\":\"fc\",\"name\":\"t\",\"arguments\":\"\"}}\n\n",
                "data: {\"type\":\"response.function_call_arguments.delta\",\"output_index\":0,\"delta\":\"{\\\"a\\\":\"}\n\n",
                "data: {\"type\":\"response.function_call_arguments.done\",\"output_index\":0,\"arguments\":\"{\\\"a\\\":1}\"}\n\n",
                "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"usage\":{\"input_tokens\":3,\"output_tokens\":4,\"input_tokens_details\":{\"cached_tokens\":1}}}}\n\n"
            );
            write!(s, "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\n\r\n{}", body.len(), body).unwrap();
        });
        let p = ResponsesProvider::new("unused", None)
            .with_codex(true)
            .with_credentials(std::sync::Arc::new(StaticResolver {
                base_url: format!("http://{}", addr),
            }));
        let mut stream = p.stream(&req()).await.unwrap();
        let mut seen = Vec::new();
        while let Some(ev) = stream.next().await {
            seen.push(ev.unwrap());
        }
        let raw = rx.recv().unwrap();
        assert!(raw.starts_with("POST /codex/responses "));
        assert!(raw
            .to_ascii_lowercase()
            .contains("authorization: bearer tok"));
        assert!(raw
            .to_ascii_lowercase()
            .contains("chatgpt-account-id: acct"));
        assert!(raw.contains("\"store\":false"));
        assert!(seen
            .iter()
            .any(|e| matches!(e, StreamEvent::ToolCallStart{ name, .. } if name == "t")));
        assert_eq!(
            seen.iter()
                .filter(|e| matches!(e, StreamEvent::ToolCallStart { .. }))
                .count(),
            1
        );
        assert!(seen
            .iter()
            .any(|e| matches!(e, StreamEvent::MessageEnd { .. })));
    }
}
