//! Radius/pi-messages provider runtime.
//!
//! Minimal implementation of pi-ai's `pi-messages` wire protocol for the Radius
//! gateway: POST `{ model, context, options }` to `<baseUrl>/messages` and
//! consume assistant-message SSE events.

use super::{http_post_sse, ChatRequest, EventHandler, ProviderError, ResponseStream, StreamEvent};
use crate::message::{ContentBlock, Message, Role, StopReason, Usage};
use crate::provider::credentials::{CredentialResolver, CredentialSource};
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

pub const DEFAULT_PI_MESSAGES_BASE_URL: &str = "https://radius.pi.dev/v1";

pub struct PiProvider {
    client: reqwest::Client,
    credentials: CredentialSource,
    base_url: Option<String>,
    compat: Value,
}

impl PiProvider {
    pub fn new(api_key: impl Into<String>, base_url: Option<String>) -> Self {
        Self {
            client: super::shared_client(),
            credentials: CredentialSource::new(api_key),
            base_url,
            compat: Value::Null,
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

    pub async fn stream(&self, req: &ChatRequest) -> Result<ResponseStream, ProviderError> {
        let cred = self.credentials.resolve().await?;
        let url = pi_messages_url(cred.base_url.clone().or_else(|| self.base_url.clone()));
        let body = build_pi_messages_body(req, &self.compat);
        let mut headers = cred.request_headers(req);
        if !cred.api_key.is_empty() {
            if cred.bearer_auth
                || !headers
                    .iter()
                    .any(|(k, _)| k.eq_ignore_ascii_case("authorization"))
            {
                headers.push(("authorization".into(), format!("Bearer {}", cred.api_key)));
            }
        }
        headers.push(("accept".into(), "text/event-stream".into()));
        headers.push(("content-type".into(), "application/json".into()));
        let mut state = PiEventState::default();
        let handler: EventHandler =
            Box::new(move |payload, out| pi_messages_event(payload, out, &mut state));
        http_post_sse(&self.client, &url, headers, body, handler).await
    }
}

fn pi_messages_url(base: Option<String>) -> String {
    let b = base
        .unwrap_or_else(|| DEFAULT_PI_MESSAGES_BASE_URL.into())
        .trim_end_matches('/')
        .to_string();
    if b.ends_with("/messages") {
        b
    } else {
        format!("{b}/messages")
    }
}

pub fn build_pi_messages_body(req: &ChatRequest, _compat: &Value) -> Value {
    let mut messages = Vec::new();
    if !req.system.is_empty() || !req.tools.is_empty() {
        messages.push(json!({
            "role": "system",
            "content": req.system,
            "toolsAdded": tools(req),
            "timestamp": crate::message::now_millis(),
        }));
    }
    for m in req.messages.iter() {
        messages.push(convert_message(m));
    }

    let mut options = Map::new();
    options.insert("maxTokens".into(), json!(req.max_tokens));
    options.insert("toolChoice".into(), json!("auto"));
    if let Some(t) = req.temperature {
        options.insert("temperature".into(), json!(t));
    }
    if let Some(level) = req
        .thinking
        .filter(|l| *l != crate::provider::ThinkingLevel::Off)
    {
        options.insert("reasoning".into(), json!(level.as_str()));
    }
    if !req.session_id.is_empty() {
        options.insert("sessionId".into(), json!(req.session_id));
    }
    if req.cache.sticky_ttl_seconds.is_some() {
        options.insert("cacheRetention".into(), json!("long"));
    }

    json!({
        "model": req.model,
        "context": { "messages": messages },
        "options": Value::Object(options),
    })
}

fn tools(req: &ChatRequest) -> Vec<Value> {
    req.tools
        .iter()
        .map(|t| json!({"name":t.name,"description":t.description,"parameters":t.parameters}))
        .collect()
}

fn convert_message(m: &Message) -> Value {
    match m.role {
        Role::System => json!({"role":"system","content":m.text(),"timestamp":m.timestamp}),
        Role::User => {
            json!({"role":"user","content":content(&m.content, false),"timestamp":m.timestamp})
        }
        Role::Assistant => {
            json!({"role":"assistant","content":content(&m.content, true),"api":"pi-messages","provider":"radius","model":m.model.clone().unwrap_or_default(),"usage":usage_value(m.usage.unwrap_or_default()),"stopReason":stop_reason(m.stop_reason.unwrap_or(StopReason::Stop)),"timestamp":m.timestamp})
        }
        Role::ToolResult => {
            json!({"role":"toolResult","toolCallId":m.tool_call_id.clone().unwrap_or_default(),"toolName":m.tool_name.clone().unwrap_or_default(),"content":[{"type":"text","text":m.text()}],"isError":m.is_error,"timestamp":m.timestamp})
        }
    }
}

fn content(blocks: &[ContentBlock], assistant: bool) -> Value {
    let arr: Vec<Value> = blocks
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Text { text } => Some(json!({"type":"text","text":text})),
            ContentBlock::Thinking { text, signature } => {
                Some(json!({"type":"thinking","thinking":text,"thinkingSignature":signature}))
            }
            ContentBlock::Image { media_type, data } if !assistant => {
                Some(json!({"type":"image","data":data,"mimeType":media_type}))
            }
            ContentBlock::ToolCall(c) if assistant => {
                Some(json!({"type":"toolCall","id":c.id,"name":c.name,"arguments":c.arguments}))
            }
            _ => None,
        })
        .collect();
    Value::Array(arr)
}

fn usage_value(u: Usage) -> Value {
    json!({"input":u.input,"output":u.output,"cacheRead":u.cache_read,"cacheWrite":u.cache_write,"totalTokens":u.total(),"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}})
}
fn stop_reason(r: StopReason) -> &'static str {
    match r {
        StopReason::Stop => "stop",
        StopReason::ToolUse => "toolUse",
        StopReason::Length => "length",
        StopReason::Aborted => "aborted",
        StopReason::Error => "error",
    }
}
fn parse_stop(s: &str) -> StopReason {
    match s {
        "toolUse" | "tool_use" => StopReason::ToolUse,
        "length" => StopReason::Length,
        "aborted" => StopReason::Aborted,
        "error" => StopReason::Error,
        _ => StopReason::Stop,
    }
}

#[derive(Default)]
pub struct PiEventState {
    text: HashMap<usize, String>,
    thinking: HashMap<usize, String>,
    tool_args: HashMap<usize, String>,
    tool_started: HashSet<usize>,
    terminal: bool,
}

pub fn pi_messages_event(payload: &str, out: &mut Vec<StreamEvent>, state: &mut PiEventState) {
    if payload == "[DONE]" {
        return;
    }
    let v: Value = match serde_json::from_str(payload) {
        Ok(v) => v,
        Err(e) => {
            out.push(StreamEvent::FatalError(format!(
                "invalid pi-messages event: {e}"
            )));
            return;
        }
    };
    match v.get("type").and_then(|t| t.as_str()).unwrap_or("") {
        "text_delta" => {
            if let (Some(i), Some(d)) = (idx(&v), v.get("delta").and_then(|d| d.as_str())) {
                state.text.entry(i).or_default().push_str(d);
                out.push(StreamEvent::TextDelta(d.into()));
            }
        }
        "text_end" => {
            if let (Some(i), Some(c)) = (idx(&v), v.get("content").and_then(|d| d.as_str())) {
                suffix_text(&mut state.text, i, c, out, false);
            }
        }
        "thinking_delta" => {
            if let (Some(i), Some(d)) = (idx(&v), v.get("delta").and_then(|d| d.as_str())) {
                state.thinking.entry(i).or_default().push_str(d);
                out.push(StreamEvent::ThinkingDelta(d.into()));
            }
        }
        "thinking_end" => {
            if let (Some(i), Some(c)) = (idx(&v), v.get("content").and_then(|d| d.as_str())) {
                suffix_text(&mut state.thinking, i, c, out, true);
                if let Some(sig) = v
                    .get("contentSignature")
                    .and_then(|s| s.as_str())
                    .filter(|s| !s.is_empty())
                {
                    out.push(StreamEvent::ThinkingSignatureDelta(sig.into()));
                }
            }
        }
        "toolcall_start" => {
            if let Some(i) = idx(&v) {
                start_tool(i, &v, out, state);
            }
        }
        "toolcall_delta" => {
            if let (Some(i), Some(d)) = (idx(&v), v.get("delta").and_then(|d| d.as_str())) {
                state.tool_args.entry(i).or_default().push_str(d);
                out.push(StreamEvent::ToolCallDelta {
                    index: i,
                    json_fragment: d.into(),
                });
            }
        }
        "toolcall_end" => {
            if let Some(i) = idx(&v) {
                end_tool(i, &v, out, state);
            }
        }
        "done" => terminal(&v, out, state, false),
        "error" => terminal(&v, out, state, true),
        _ => {}
    }
}
fn idx(v: &Value) -> Option<usize> {
    v.get("contentIndex")
        .and_then(|i| i.as_u64())
        .map(|i| i as usize)
}
fn suffix_text(
    map: &mut HashMap<usize, String>,
    i: usize,
    c: &str,
    out: &mut Vec<StreamEvent>,
    thinking: bool,
) {
    let prior = map.entry(i).or_default();
    if let Some(rest) = c.strip_prefix(prior.as_str()) {
        if !rest.is_empty() {
            if thinking {
                out.push(StreamEvent::ThinkingDelta(rest.into()))
            } else {
                out.push(StreamEvent::TextDelta(rest.into()))
            }
        }
    }
    *prior = c.to_string();
}
fn start_tool(i: usize, v: &Value, out: &mut Vec<StreamEvent>, st: &mut PiEventState) {
    if st.tool_started.insert(i) {
        let tc = v.get("toolCall");
        let id = v
            .get("id")
            .and_then(|s| s.as_str())
            .or_else(|| tc.and_then(|t| t.get("id")).and_then(|s| s.as_str()))
            .unwrap_or_default();
        let name = v
            .get("toolName")
            .and_then(|s| s.as_str())
            .or_else(|| v.get("name").and_then(|s| s.as_str()))
            .or_else(|| tc.and_then(|t| t.get("name")).and_then(|s| s.as_str()))
            .unwrap_or_default();
        out.push(StreamEvent::ToolCallStart {
            index: i,
            id: id.into(),
            name: name.into(),
        });
    }
}
fn end_tool(i: usize, v: &Value, out: &mut Vec<StreamEvent>, st: &mut PiEventState) {
    if !st.tool_started.contains(&i) {
        start_tool(i, v, out, st);
    }
    let args = v
        .get("arguments")
        .or_else(|| v.get("toolCall").and_then(|t| t.get("arguments")));
    if let Some(args) = args {
        if let Ok(s) = serde_json::to_string(args) {
            let prior = st.tool_args.entry(i).or_default();
            if let Some(rest) = s.strip_prefix(prior.as_str()) {
                if !rest.is_empty() {
                    out.push(StreamEvent::ToolCallDelta {
                        index: i,
                        json_fragment: rest.into(),
                    });
                }
            }
            *prior = s;
        }
    }
}
fn terminal(v: &Value, out: &mut Vec<StreamEvent>, st: &mut PiEventState, err: bool) {
    if st.terminal {
        return;
    }
    st.terminal = true;
    let msg = if err {
        v.get("error")
    } else {
        v.get("message")
    };
    if let Some(u) = msg.and_then(|m| m.get("usage")).or_else(|| v.get("usage")) {
        out.push(StreamEvent::Usage(parse_usage(u)));
    }
    if err {
        out.push(StreamEvent::FatalError(
            error_text(v).unwrap_or_else(|| "pi-messages stream error".into()),
        ));
    } else {
        out.push(StreamEvent::MessageEnd {
            stop_reason: v
                .get("reason")
                .and_then(|r| r.as_str())
                .map(parse_stop)
                .unwrap_or(StopReason::Stop),
        });
    }
}
fn error_text(v: &Value) -> Option<String> {
    v.get("errorMessage")
        .and_then(|m| m.as_str())
        .or_else(|| v.get("message").and_then(|m| m.as_str()))
        .or_else(|| {
            v.get("error")
                .and_then(|e| e.get("errorMessage"))
                .and_then(|m| m.as_str())
        })
        .or_else(|| {
            v.get("error")
                .and_then(|e| e.get("message"))
                .and_then(|m| m.as_str())
        })
        .map(str::to_string)
}
fn parse_usage(u: &Value) -> Usage {
    let g = |k: &str| u.get(k).and_then(|x| x.as_u64()).unwrap_or(0);
    Usage {
        input: g("input"),
        output: g("output"),
        cache_read: g("cacheRead"),
        cache_write: g("cacheWrite"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{CacheHints, ThinkingLevel, ToolDef};
    fn req() -> ChatRequest {
        ChatRequest {
            system: "sys".into(),
            messages: vec![Message::user_text("hi")].into(),
            tools: vec![ToolDef {
                name: "t".into(),
                description: "d".into(),
                parameters: json!({"type":"object"}),
            }]
            .into(),
            model: "r".into(),
            max_tokens: 9,
            temperature: Some(0.1),
            cache: CacheHints::default(),
            thinking: None,
            session_id: "s".into(),
        }
    }
    #[test]
    fn payload_has_pi_context_tools_and_omits_undefined_options() {
        let mut r = req();
        r.temperature = None;
        r.session_id.clear();
        r.thinking = Some(ThinkingLevel::Off);
        let b = build_pi_messages_body(&r, &Value::Null);
        assert_eq!(b["model"], "r");
        assert_eq!(b["context"]["messages"][0]["toolsAdded"][0]["name"], "t");
        assert!(b["options"].get("temperature").is_none());
        assert!(b["options"].get("reasoning").is_none());
        assert!(b["options"].get("sessionId").is_none());
    }
    #[test]
    fn upstream_tool_events_start_with_raw_fields_and_round_trip_args() {
        let mut out = vec![];
        let mut st = PiEventState::default();
        pi_messages_event(
            r#"{"type":"toolcall_start","contentIndex":0,"id":"call_1","toolName":"bash"}"#,
            &mut out,
            &mut st,
        );
        pi_messages_event(
            r#"{"type":"toolcall_delta","contentIndex":0,"delta":"{\"command\":"}"#,
            &mut out,
            &mut st,
        );
        pi_messages_event(
            r#"{"type":"toolcall_end","contentIndex":0,"id":"call_1","name":"bash","arguments":{"command":"pwd"}}"#,
            &mut out,
            &mut st,
        );
        pi_messages_event(
            r#"{"type":"done","reason":"toolUse","usage":{"input":1,"output":2,"cacheRead":3,"cacheWrite":4}}"#,
            &mut out,
            &mut st,
        );
        assert!(
            matches!(out[0], StreamEvent::ToolCallStart{ ref id, ref name, .. } if id == "call_1" && name == "bash")
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
        assert_eq!(args, "{\"command\":\"pwd\"}");
        assert_eq!(
            out.iter()
                .filter(|e| matches!(e, StreamEvent::ToolCallStart { .. }))
                .count(),
            1
        );
        assert!(matches!(
            out.last().unwrap(),
            StreamEvent::MessageEnd {
                stop_reason: StopReason::ToolUse
            }
        ));
    }
    #[test]
    fn thinking_end_emits_signature_and_no_duplicate_text() {
        let mut out = vec![];
        let mut st = PiEventState::default();
        pi_messages_event(
            r#"{"type":"thinking_delta","contentIndex":0,"delta":"thi"}"#,
            &mut out,
            &mut st,
        );
        pi_messages_event(
            r#"{"type":"thinking_end","contentIndex":0,"content":"think","contentSignature":"sig"}"#,
            &mut out,
            &mut st,
        );
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
        assert_eq!(thinking, "think");
        assert!(out
            .iter()
            .any(|e| matches!(e, StreamEvent::ThinkingSignatureDelta(s) if s == "sig")));
    }
    #[test]
    fn error_event_preserves_text() {
        let mut out = vec![];
        let mut st = PiEventState::default();
        pi_messages_event(
            r#"{"type":"error","reason":"error","error":{"errorMessage":"radius said no"}}"#,
            &mut out,
            &mut st,
        );
        assert!(matches!(out.last().unwrap(), StreamEvent::FatalError(s) if s == "radius said no"));
    }

    struct StaticResolver {
        base_url: String,
    }
    impl crate::provider::credentials::CredentialResolver for StaticResolver {
        fn resolve(&self) -> crate::provider::credentials::CredentialFuture<'_> {
            Box::pin(async move {
                Ok(crate::provider::credentials::ResolvedCredential {
                    api_key: "tok".into(),
                    bearer_auth: true,
                    base_url: Some(self.base_url.clone()),
                    headers: vec![("x-extra".into(), "yes".into())],
                    ..Default::default()
                })
            })
        }
    }

    #[tokio::test]
    async fn mocked_http_sends_radius_url_headers_body_and_streams() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::sync::mpsc;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut buf = Vec::new();
            let mut tmp = [0u8; 1024];
            loop {
                let n = s.read(&mut tmp).unwrap();
                buf.extend_from_slice(&tmp[..n]);
                let raw = String::from_utf8_lossy(&buf);
                if let Some(split) = raw.find("\r\n\r\n") {
                    let len = raw[..split]
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length: ")
                                .and_then(|v| v.trim().parse::<usize>().ok())
                        })
                        .unwrap_or(0);
                    if buf.len() >= split + 4 + len {
                        break;
                    }
                }
            }
            tx.send(String::from_utf8_lossy(&buf).to_string()).unwrap();
            let body = concat!(
                "data: {\"type\":\"text_delta\",\"contentIndex\":0,\"delta\":\"hi\"}\n\n",
                "data: {\"type\":\"done\",\"reason\":\"stop\",\"usage\":{\"input\":1,\"output\":2,\"cacheRead\":0,\"cacheWrite\":0}}\n\n"
            );
            write!(s, "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\n\r\n{}", body.len(), body).unwrap();
        });
        let p =
            PiProvider::new("unused", None).with_credentials(std::sync::Arc::new(StaticResolver {
                base_url: format!("http://{}", addr),
            }));
        let mut stream = p.stream(&req()).await.unwrap();
        let mut seen = Vec::new();
        while let Some(ev) = stream.next().await {
            seen.push(ev.unwrap());
        }
        let raw = rx.recv().unwrap();
        assert!(raw.starts_with("POST /messages "));
        assert!(raw
            .to_ascii_lowercase()
            .contains("authorization: bearer tok"));
        assert!(raw.to_ascii_lowercase().contains("x-extra: yes"));
        assert!(raw.contains("\"context\""));
        assert!(seen
            .iter()
            .any(|e| matches!(e, StreamEvent::TextDelta(s) if s == "hi")));
        assert!(seen.iter().any(|e| matches!(
            e,
            StreamEvent::MessageEnd {
                stop_reason: StopReason::Stop
            }
        )));
    }
}
