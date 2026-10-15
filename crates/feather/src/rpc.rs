//! pi-compatible JSONL command/response and session-event framing.
//! Unsupported commands return explicit errors, never fake successful responses.
use crate::models;
use feather_core::{
    agent::{Agent, AgentEvent},
    message::{Api, ContentBlock, Message, Role, StopReason, Usage},
    provider::ThinkingLevel,
    session::Session,
};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt};

fn response(command: &Value, result: Result<Option<Value>, String>) -> Value {
    let mut value = json!({"type":"response", "command":command["type"].as_str().unwrap_or("parse"), "success":result.is_ok()});
    if let Some(id) = command.get("id") {
        value["id"] = id.clone();
    }
    match result {
        Ok(Some(data)) => value["data"] = data,
        Err(error) => value["error"] = json!(error),
        _ => {}
    }
    value
}
async fn emit(out: &mut (impl AsyncWrite + Unpin), value: &Value) -> anyhow::Result<()> {
    let mut bytes = serde_json::to_vec(value)?;
    bytes.push(b'\n');
    out.write_all(&bytes).await?;
    out.flush().await?;
    Ok(())
}
fn model_cost(model: &str) -> Option<models::ModelCost> {
    // Called once per assistant message while encoding a transcript, so it must
    // not clone the whole catalog (see `models::with_catalog`).
    models::cost_for(model)
}
fn usage_cost(u: Usage, cost: Option<&models::ModelCost>) -> Value {
    if let Some(c) = cost {
        let input = u.input as f64 * c.input.unwrap_or(0.0) / 1_000_000.0;
        let output = u.output as f64 * c.output.unwrap_or(0.0) / 1_000_000.0;
        let cache_read = u.cache_read as f64 * c.cache_read.unwrap_or(0.0) / 1_000_000.0;
        let cache_write = u.cache_write as f64 * c.cache_write.unwrap_or(0.0) / 1_000_000.0;
        json!({"input":input,"output":output,"cacheRead":cache_read,"cacheWrite":cache_write,"total":input+output+cache_read+cache_write})
    } else {
        Value::Null
    }
}
fn zero_cost() -> Value {
    static ZERO: std::sync::OnceLock<Value> = std::sync::OnceLock::new();
    ZERO.get_or_init(
        || json!({"input":0.0,"output":0.0,"cacheRead":0.0,"cacheWrite":0.0,"total":0.0}),
    )
    .clone()
}
/// pi's `Usage` wire value. `cost` stays a zero object so pi clients that read
/// `cost.total` keep working, but `pricingKnown: false` marks those zeros as
/// "not priced" rather than a real free model. Callers with a resolved message
/// model overwrite both `cost` and `pricingKnown`.
fn wire_usage(u: Usage) -> Value {
    json!({"input":u.input,"output":u.output,"cacheRead":u.cache_read,"cacheWrite":u.cache_write,"totalTokens":u.total(),"cost":zero_cost(),"pricingKnown":false})
}
/// The all-zero usage attached to every streaming delta. Deltas arrive per
/// token, and the object never varies, so it is built once and cloned.
fn wire_usage_zero() -> Value {
    static ZERO: std::sync::OnceLock<Value> = std::sync::OnceLock::new();
    ZERO.get_or_init(|| wire_usage(Usage::default())).clone()
}
fn model_object(
    provider: &str,
    p: &models::ProviderEntry,
    m: &models::ModelEntry,
    global_context: u64,
) -> Value {
    let mut v = json!({
        "id": m.id, "name": m.name.clone().unwrap_or_else(|| m.id.clone()),
        "provider": provider, "api": p.api.clone().unwrap_or_else(|| "openai-completions".into()),
        "reasoning": m.reasoning, "input": m.input,
        // What the agent will run with (global context capped by the model) and
        // the model's own advertised limit, for clients that show both.
        "contextWindow": models::effective_context_window(global_context, Some(m)),
        "modelContextWindow": m.context_window, "maxTokens": m.max_tokens,
    });
    if let Some(b) = &p.base_url {
        v["baseUrl"] = json!(b);
    }
    if let Some(c) = &m.cost {
        v["cost"] = json!(c);
    }
    v["thinkingLevels"] = json!(models::thinking_levels(m, p));
    v
}
fn current_model_object(agent: &Agent) -> Value {
    let effective = agent.config().context_window;
    if let Some(cat) = models::load() {
        if let Some((pid, p, m)) = models::find(&cat, &agent.model_id()) {
            return model_object(pid, p, m, effective);
        }
    }
    json!({"id":agent.model_id(),"api":api_name(agent.config().api),"contextWindow":effective,"maxTokens":agent.config().max_tokens})
}
fn available_thinking_levels(agent: &Agent) -> Vec<String> {
    if let Some(cat) = models::load() {
        if let Some((_, p, m)) = models::find(&cat, &agent.model_id()) {
            return models::thinking_levels(m, p);
        }
    }
    models::DEFAULT_EFFORTS
        .iter()
        .map(|s| s.to_string())
        .collect()
}
/// Thinking levels from a cached model object; used while a run owns the agent.
fn levels_for_model(model: &Value) -> Vec<String> {
    if model["reasoning"].as_bool() == Some(false) {
        return vec!["off".into()];
    }
    model["thinkingLevels"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect::<Vec<_>>()
        })
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| {
            models::DEFAULT_EFFORTS
                .iter()
                .map(|s| s.to_string())
                .collect()
        })
}
fn clamp_level(agent: &Agent, level: ThinkingLevel) -> ThinkingLevel {
    if let Some(cat) = models::load() {
        if let Some((_, p, m)) = models::find(&cat, &agent.model_id()) {
            return models::clamp_thinking(m, p, level);
        }
    }
    let levels = available_thinking_levels(agent);
    if levels.iter().any(|l| l == level.as_str()) {
        return level;
    }
    for candidate in [
        ThinkingLevel::High,
        ThinkingLevel::Medium,
        ThinkingLevel::Low,
        ThinkingLevel::Minimal,
        ThinkingLevel::Off,
    ] {
        if levels.iter().any(|l| l == candidate.as_str()) {
            return candidate;
        }
    }
    ThinkingLevel::Off
}
/// Sum token usage across every message and price it per message model so a
/// session that switched models is costed correctly. `pricingKnown` is false
/// when any usage-bearing message's model has no catalog price, so the
/// pi-compatible zero `cost` is not mistaken for a real free total.
fn compute_stats(
    msgs: &[Message],
    session_file: String,
    session_id: &str,
    context_window: u64,
) -> Value {
    let mut user = 0u64;
    let mut assistant = 0u64;
    let mut tools = 0u64;
    let mut tool_calls = 0u64;
    let mut total_usage = Usage::default();
    let mut cost = 0.0f64;
    let mut pricing_known = true;
    for m in msgs {
        match m.role {
            Role::User => user += 1,
            Role::Assistant => {
                assistant += 1;
                tool_calls += m.tool_calls().len() as u64;
            }
            Role::ToolResult => tools += 1,
            _ => {}
        }
        if let Some(u) = m.usage {
            total_usage.input += u.input;
            total_usage.output += u.output;
            total_usage.cache_read += u.cache_read;
            total_usage.cache_write += u.cache_write;
            match m.model.as_deref().and_then(model_cost) {
                Some(c) => cost += usage_cost(u, Some(&c))["total"].as_f64().unwrap_or(0.0),
                None if u.total() > 0 => pricing_known = false,
                None => {}
            }
        }
    }
    let ctx = feather_core::tokenizer::estimate_context_tokens(msgs);
    let mut v = json!({
        "sessionFile": session_file,
        "sessionId": session_id,
        "userMessages": user,
        "assistantMessages": assistant,
        "toolCalls": tool_calls,
        "toolResults": tools,
        "totalMessages": msgs.len(),
        "tokens": {
            "input": total_usage.input,
            "output": total_usage.output,
            "cacheRead": total_usage.cache_read,
            "cacheWrite": total_usage.cache_write,
            "total": total_usage.total(),
        },
        "cost": cost,
        "pricingKnown": pricing_known,
    });
    if context_window > 0 {
        v["contextUsage"] = json!({
            "tokens": ctx,
            "contextWindow": context_window,
            "percent": (ctx as f64 * 100.0 / context_window as f64),
        });
    }
    v
}
fn session_stats(agent: &Agent) -> Value {
    compute_stats(
        agent.session().messages(),
        agent
            .session()
            .dir()
            .join("session.redb")
            .display()
            .to_string(),
        agent.session().id(),
        agent.config().context_window,
    )
}
/// Stats for `get_session_stats` while a run/compaction owns the agent: the
/// numbers come from the busy snapshot plus the cached model object.
fn cached_stats(state: &Value, msgs: &[Message]) -> Value {
    compute_stats(
        msgs,
        state["sessionFile"].as_str().unwrap_or("").to_string(),
        state["sessionId"].as_str().unwrap_or(""),
        state["model"]["contextWindow"].as_u64().unwrap_or(0),
    )
}
fn export_html(agent: &Agent, output: Option<&str>) -> Result<String, String> {
    let path = output
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| agent.session().dir().join("session.html"));
    let mut html = String::from(
        "<!doctype html><meta charset=\"utf-8\"><title>Feather session</title><body>\n",
    );
    for m in agent.session().messages() {
        html.push_str(&format!(
            "<section><h3>{}</h3><pre>{}</pre></section>\n",
            m.role_name(),
            html_escape(&m.text())
        ));
    }
    html.push_str("</body>\n");
    std::fs::write(&path, html).map_err(|e| e.to_string())?;
    Ok(path.display().to_string())
}
fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}
/// Discover prompt templates and skills into pi's `get_commands` shape.
/// Feather has no TypeScript extension host, so `source: "extension"` never
/// appears; built-in TUI commands stay out of the list just like pi.
fn resource_scope(path: &Path, home: &Path, cwd: &Path) -> &'static str {
    if path.starts_with(home) {
        "user"
    } else if path.starts_with(cwd) {
        "project"
    } else {
        "temporary"
    }
}
fn commands_object(home: &Path, cwd: &Path) -> Value {
    // `config::resources_for` is the one authority for settings-scoped
    // `+`/`-`/glob resource paths; keep `get_commands` in lockstep with the
    // prompt-expansion path used by the CLI.
    let loaded = crate::config::resources_for(cwd, home);
    let mut commands = Vec::new();
    for p in &loaded.prompts {
        commands.push(json!({
            "name": p.name,
            "description": p.description,
            "source": "prompt",
            "sourceInfo": {
                "path": p.file_path,
                "source": "local",
                "scope": resource_scope(&p.file_path, home, cwd),
                "origin": "top-level",
            }
        }));
    }
    for s in &loaded.skills {
        commands.push(json!({
            "name": format!("skill:{}", s.name),
            "description": s.description,
            "source": "skill",
            "sourceInfo": {
                "path": s.file_path,
                "source": "local",
                "scope": resource_scope(&s.file_path, home, cwd),
                "origin": "top-level",
            }
        }));
    }
    json!({"commands": commands})
}
fn api_name(api: Api) -> &'static str {
    match api {
        Api::AnthropicMessages => "anthropic-messages",
        Api::OpenaiCompletions => "openai-completions",
        Api::OpenaiResponses => "openai-responses",
        Api::OpenaiCodexResponses => "openai-codex-responses",
        Api::PiMessages => "pi-messages",
        Api::Faux => "faux",
    }
}
pub fn wire_message(m: &Message) -> Value {
    let content: Vec<_> = m
        .content
        .iter()
        .map(|b| match b {
            ContentBlock::Text { text } => json!({"type":"text","text":text}),
            ContentBlock::Image { media_type, data } => {
                json!({"type":"image","data":data,"mimeType":media_type})
            }
            ContentBlock::Thinking { text, signature } => {
                let mut b = json!({"type":"thinking","thinking":text});
                if let Some(s) = signature {
                    b["thinkingSignature"] = json!(s);
                }
                b
            }
            ContentBlock::ToolCall(call) => {
                json!({"type":"toolCall","id":call.id,"name":call.name,"arguments":call.arguments})
            }
        })
        .collect();
    let mut v = json!({"role":match m.role {Role::User=>"user",Role::Assistant=>"assistant",Role::System=>"system",Role::ToolResult=>"toolResult"},"content":content,"timestamp":m.timestamp});
    if m.role == Role::Assistant {
        let api = m.api.unwrap_or(Api::Faux);
        v["api"] = json!(api_name(api));
        v["provider"] = json!(match api {
            Api::AnthropicMessages => "anthropic",
            Api::OpenaiCompletions => "openai",
            Api::OpenaiResponses => "openai",
            Api::OpenaiCodexResponses => "openai-codex",
            Api::PiMessages => "radius",
            Api::Faux => "faux",
        });
        v["model"] = json!(m.model);
        let u = m.usage.unwrap_or_default();
        let mut usage = wire_usage(u);
        if let Some(model) = m.model.as_deref() {
            if let Some(c) = model_cost(model) {
                usage["cost"] = usage_cost(u, Some(&c));
                usage["pricingKnown"] = json!(true);
            }
        }
        v["usage"] = usage;
        v["stopReason"] = json!(match m.stop_reason.unwrap_or(StopReason::Stop) {
            StopReason::ToolUse => "toolUse",
            StopReason::Stop => "stop",
            StopReason::Aborted => "aborted",
            StopReason::Length => "length",
            StopReason::Error => "error",
        });
    }
    if m.role == Role::ToolResult {
        v["toolCallId"] = json!(m.tool_call_id);
        v["toolName"] = json!(m.tool_name);
        v["isError"] = json!(m.is_error);
    }
    v
}
fn user_message(command: &Value) -> Result<Message, String> {
    let text = command["message"]
        .as_str()
        .ok_or("message must be a string")?;
    let mut message = Message::user_text(text);
    if let Some(images) = command.get("images") {
        for image in images.as_array().ok_or("images must be an array")? {
            let mime = image["mimeType"]
                .as_str()
                .ok_or("image.mimeType must be a string")?;
            if !["image/png", "image/jpeg", "image/gif", "image/webp"].contains(&mime) {
                return Err("unsupported image MIME type".into());
            }
            message.content.push(ContentBlock::Image {
                media_type: mime.into(),
                data: image["data"]
                    .as_str()
                    .ok_or("image.data must be a string")?
                    .into(),
            });
        }
    }
    Ok(message)
}
/// Session names come from `metadata.redb`. `state()` runs per command and per
/// settle, so the lookup is memoized on the database's (mtime, len) stamp
/// rather than reopening redb every time.
type NameCache =
    std::sync::Mutex<std::collections::HashMap<PathBuf, (Option<(u64, u64)>, Option<String>)>>;
static SESSION_NAMES: std::sync::OnceLock<NameCache> = std::sync::OnceLock::new();

fn file_stamp(path: &Path) -> Option<(u64, u64)> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime = meta
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_millis() as u64;
    Some((mtime, meta.len()))
}

/// Display-name metadata lives in redb; legacy name JSON is archived on import.
fn read_session_name(dir: &Path) -> Option<String> {
    let stamp = file_stamp(&dir.join("metadata.redb"));
    let cache =
        SESSION_NAMES.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()));
    if let Ok(guard) = cache.lock() {
        if let Some((cached_stamp, name)) = guard.get(dir) {
            if *cached_stamp == stamp {
                return name.clone();
            }
        }
    }
    let name = read_session_name_uncached(dir);
    if let Ok(mut guard) = cache.lock() {
        guard.insert(dir.to_path_buf(), (stamp, name.clone()));
    }
    name
}

fn read_session_name_uncached(dir: &Path) -> Option<String> {
    let store = feather_core::records::Records::open(&dir.join("metadata.redb")).ok()?;
    if let Some(name) = store.get::<String>("name").ok()? {
        return Some(name);
    }
    let legacy = dir.join("session-name.json");
    let value: Value = serde_json::from_slice(&std::fs::read(&legacy).ok()?).ok()?;
    let name = value["name"].as_str()?.to_string();
    store.put("name", &name).ok()?;
    std::fs::rename(&legacy, legacy.with_extension("json.legacy")).ok()?;
    Some(name)
}
fn write_session_name(dir: &Path, name: &str) -> std::io::Result<()> {
    let result = feather_core::records::Records::open(&dir.join("metadata.redb"))
        .and_then(|s| s.put("name", &name))
        .map_err(std::io::Error::other);
    // The stamp check cannot see a same-length rename within one mtime tick, so
    // drop the entry explicitly.
    if let Some(cache) = SESSION_NAMES.get() {
        if let Ok(mut guard) = cache.lock() {
            guard.remove(dir);
        }
    }
    result
}
/// Directory holding the active transcript, from the live agent when idle or
/// the cached session file path while a run owns the agent.
fn session_dir(idle: Option<&Agent>, cached_state: &Value) -> PathBuf {
    if let Some(agent) = idle {
        return agent.session().dir().to_path_buf();
    }
    cached_state["sessionFile"]
        .as_str()
        .map(Path::new)
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .unwrap_or_default()
}
/// One `type:"message"` line from the raw append-only transcript.
struct TranscriptEntry {
    id: String,
    timestamp: u64,
    message: Message,
}
fn read_transcript(dir: &Path) -> Result<Vec<TranscriptEntry>, String> {
    feather_core::session_store::SessionStore::open(dir)
        .map_err(|e| e.to_string())?
        .raw_entries()
        .map_err(|e| e.to_string())
        .map(|entries| {
            entries
                .into_iter()
                .map(|e| TranscriptEntry {
                    id: e.id,
                    timestamp: e.timestamp,
                    message: e.message,
                })
                .collect()
        })
}
/// Map the linear transcript to pi-style entries: `parentId` chains to the
/// previous entry (a degenerate tree, since feather has no branches).
/// Encode transcript entries, optionally starting at `start` (the entry the
/// client already has). Only the requested tail is encoded: building every
/// entry first and dropping the prefix costs a full wire serialization of the
/// whole transcript, which is the hottest path in `get_entries`.
fn entries_payload(entries: &[TranscriptEntry], start: usize) -> Vec<Value> {
    let start = start.min(entries.len());
    let tail = &entries[start..];
    let mut out = Vec::with_capacity(tail.len());
    // The parent of the first returned entry is the last entry the client saw.
    let mut parent: Option<&str> = start
        .checked_sub(1)
        .and_then(|previous| entries.get(previous))
        .map(|entry| entry.id.as_str());
    for entry in tail {
        out.push(json!({
            "type": "message",
            "id": entry.id,
            "parentId": parent,
            "timestamp": entry.timestamp,
            "message": wire_message(&entry.message),
        }));
        parent = Some(&entry.id);
    }
    out
}
fn state(agent: &Agent) -> Value {
    let (steering, follow) = agent.input_queue().snapshot();
    let modes = agent.input_queue().modes();
    let mut v = json!({"model":current_model_object(agent),
        "thinkingLevel":agent.thinking_level().map(|l|l.as_str()).unwrap_or("off"),"isStreaming":false,"isCompacting":false,
        "sessionId":agent.session().id(),"sessionFile":agent.session().dir().join("session.redb"),"messageCount":agent.session().messages().len(),
        "pendingMessageCount":steering.len()+follow.len(),"autoCompactionEnabled":agent.config().compaction.enabled,
        "steeringMode":if modes.0 {"all"} else {"one-at-a-time"},"followUpMode":if modes.1 {"all"} else {"one-at-a-time"}});
    if let Some(name) = read_session_name(agent.session().dir()) {
        v["sessionName"] = json!(name);
    }
    v
}

#[derive(Default)]
struct EventMapper {
    text_index: Option<usize>,
    thinking_index: Option<usize>,
    next_index: usize,
    last_message: Option<Value>,
    tool_results: Vec<Value>,
    run_messages: Vec<Value>,
}

fn retry_delay_label(ms: u64) -> String {
    if ms >= 1_000 {
        let secs = ms as f64 / 1_000.0;
        if (secs.fract()).abs() < f64::EPSILON {
            format!("{}s", secs as u64)
        } else {
            format!("{secs:.1}s")
        }
    } else {
        format!("{ms}ms")
    }
}

fn sanitize_retry_reason(reason: &str) -> String {
    let compact = reason.split_whitespace().collect::<Vec<_>>().join(" ");
    let lower = compact.to_ascii_lowercase();
    if lower.contains("authorization")
        || lower.contains("api_key")
        || lower.contains("api key")
        || lower.contains("bearer ")
        || lower.contains(" secret")
        || lower.contains("sk-")
    {
        "provider error".to_string()
    } else if compact.chars().count() > 240 {
        let mut s: String = compact.chars().take(240).collect();
        s.push('…');
        s
    } else {
        compact
    }
}
impl EventMapper {
    fn map(&mut self, event: AgentEvent) -> Vec<Value> {
        let thinking = matches!(&event, AgentEvent::ThinkingDelta(_));
        match event {
            AgentEvent::TurnStart(_) => {
                self.text_index = None;
                self.thinking_index = None;
                self.next_index = 0;
                self.tool_results.clear();
                vec![
                    json!({"type":"turn_start"}),
                    json!({"type":"message_start","message":{"role":"assistant","content":[],"stopReason":"pending"}}),
                ]
            }
            AgentEvent::TextDelta(delta) | AgentEvent::ThinkingDelta(delta) => {
                let slot = if thinking {
                    &mut self.thinking_index
                } else {
                    &mut self.text_index
                };
                let mut records = Vec::new();
                let index = match *slot {
                    Some(i) => i,
                    None => {
                        let index = self.next_index;
                        self.next_index += 1;
                        *slot = Some(index);
                        records.push(json!({"type":"message_update","usage":wire_usage_zero(),"assistantMessageEvent":{"type":if thinking {"thinking_start"} else {"text_start"},"contentIndex":index}}));
                        index
                    }
                };
                records.push(json!({"type":"message_update","usage":wire_usage_zero(),"assistantMessageEvent":{"type":if thinking {"thinking_delta"} else {"text_delta"},"contentIndex":index,"delta":delta}}));
                records
            }
            AgentEvent::ResponseReset {
                attempt,
                max_attempts,
                delay_ms,
                reason,
            } => {
                self.text_index = None;
                self.thinking_index = None;
                self.next_index = 0;
                self.last_message = None;
                self.tool_results.clear();
                let reason = sanitize_retry_reason(&reason);
                let delay = retry_delay_label(delay_ms);
                vec![
                    json!({"type":"message_reset","message":{"role":"assistant","content":[],"stopReason":"pending"},"attempt":attempt,"maxAttempts":max_attempts,"delayMs":delay_ms,"reason":reason}),
                    json!({"type":"message_update","usage":wire_usage_zero(),"assistantMessageEvent":{"type":"response_reset","attempt":attempt,"maxAttempts":max_attempts,"delayMs":delay_ms,"reason":reason}}),
                    json!({"type":"notice","message":format!("Retrying response: attempt {attempt}/{max_attempts} in {delay}"),"willRetry":true,"attempt":attempt,"maxAttempts":max_attempts,"delayMs":delay_ms}),
                ]
            }
            AgentEvent::AssistantMessage(message) => {
                let v = wire_message(&message);
                self.run_messages.push(v.clone());
                if message.role == Role::User {
                    // queued steer/follow-up delivery: echo the user message
                    // (with image blocks) before the next turn starts
                    vec![
                        json!({"type":"message_start","message":v}),
                        json!({"type":"message_end","message":v}),
                    ]
                } else {
                    self.last_message = Some(v.clone());
                    vec![json!({"type":"message_end","message":v})]
                }
            }
            AgentEvent::ToolStart { call } => vec![
                json!({"type":"tool_execution_start","toolCallId":call.id,"toolName":call.name,"args":call.arguments}),
            ],
            AgentEvent::ToolProgress { call, text } => vec![
                json!({"type":"tool_execution_update","toolCallId":call.id,"toolName":call.name,"partialResult":{"content":[{"type":"text","text":text}],"details":{}}}),
            ],
            AgentEvent::ToolEnd {
                call,
                text,
                is_error,
            } => {
                let result = json!({"content":[{"type":"text","text":text}]});
                let message =
                    wire_message(&Message::tool_result(&call.id, &call.name, &text, is_error));
                self.tool_results.push(message.clone());
                self.run_messages.push(message.clone());
                vec![
                    json!({"type":"tool_execution_end","toolCallId":call.id,"toolName":call.name,"result":result,"isError":is_error}),
                    json!({"type":"message_start","message":message}),
                    json!({"type":"message_end","message":message}),
                ]
            }
            AgentEvent::AuditCommitted { .. } => Vec::new(),
            AgentEvent::TurnEnd(_) => vec![
                json!({"type":"turn_end","message":self.last_message,"toolResults":self.tool_results}),
            ],
            AgentEvent::RunEnd(_) => vec![
                json!({"type":"agent_end","messages":std::mem::take(&mut self.run_messages),"willRetry":false}),
            ],
            AgentEvent::CompactionDone {
                tokens_before,
                tokens_after,
                ..
            } => vec![
                json!({"type":"compaction_end","reason":"threshold","result":{"tokensBefore":tokens_before,"estimatedTokensAfter":tokens_after},"aborted":false,"willRetry":false}),
            ],
            AgentEvent::Error(error) => vec![json!({"type":"error","error":error})],
            AgentEvent::Notice(message) => vec![json!({"type":"notice","message":message})],
        }
    }
}

fn compact_event_records(event: AgentEvent) -> Vec<Value> {
    match event {
        AgentEvent::Error(error) => vec![json!({"type":"error","error":error})],
        AgentEvent::Notice(message) => vec![json!({"type":"notice","message":message})],
        _ => Vec::new(),
    }
}

/// The reader splits only on LF (Unicode U+2028/2029 remain JSON string data).
/// One output owner serializes records and honors pipe backpressure. Abort uses
/// the watch sender while the spawned run exclusively owns the mutable agent.
pub async fn run<R, W, F>(
    agent: Agent,
    reader: R,
    out: W,
    home: &Path,
    cwd: &Path,
    factory: F,
) -> anyhow::Result<()>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
    F: FnMut(Session) -> anyhow::Result<Agent>,
{
    run_with_control(agent, reader, out, home, cwd, factory, |_agent, command| {
        Err(format!(
            "Unsupported command: {}",
            command["type"].as_str().unwrap_or("")
        ))
    })
    .await
}

/// Keep the busy conversation snapshot in sync with delivered messages so
/// `get_messages`/`get_session_stats` stay responsive while a run/compaction
/// owns the agent.
fn absorb_event(event: &AgentEvent, cached: &mut Vec<Message>) {
    match event {
        AgentEvent::AssistantMessage(message) => cached.push(message.clone()),
        AgentEvent::ToolEnd {
            call,
            text,
            is_error,
        } => cached.push(Message::tool_result(
            call.id.clone(),
            call.name.clone(),
            text.clone(),
            *is_error,
        )),
        _ => {}
    }
}

/// Rebuild `cached` from the session only when the incremental copy
/// ([`absorb_event`]) has drifted. Besides length, compare the first message:
/// compaction rewrites `summary + kept span` and can replace exactly one prefix
/// message, preserving the total count. Avoid a full deep comparison/clone on
/// every normal settle/submit transition.
fn sync_messages(cached: &mut Vec<Message>, agent: &Agent) {
    let messages = agent.session().messages();
    let first_changed = match (cached.first(), messages.first()) {
        (Some(cached_first), Some(session_first)) => cached_first != session_first,
        (None, None) => false,
        _ => true,
    };
    if cached.len() != messages.len() || first_changed {
        *cached = messages.to_vec();
    }
}

/// Streaming messages from a spawned RPC `bash` command back to the actor.
enum BashMsg {
    Update(Value),
    Done(Value),
}
/// State while an RPC `bash` command runs: the original command (for the
/// deferred response), the abort switch, and the streaming channel.
struct BashRun {
    command: Value,
    command_text: String,
    exclude_from_context: bool,
    abort: tokio::sync::watch::Sender<bool>,
    updates: tokio::sync::mpsc::UnboundedReceiver<BashMsg>,
}
const BASH_MAX_BYTES: usize = 50 * 1024;
/// Truncate `s` to its last `max` bytes on a UTF-8 char boundary.
fn tail_bytes(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut start = s.len() - max;
    while start < s.len() && !s.is_char_boundary(start) {
        start += 1;
    }
    s[start..].to_string()
}

/// Rolling bash output buffer: keeps only the last `BASH_MAX_BYTES` bytes in
/// RAM and, once the cap is first exceeded, streams the *full* output to a
/// temp file. Previously the whole output accumulated in memory and was
/// capped only after the process exited — unbounded for chatty commands.
struct BashOutput {
    tail: String,
    total: usize,
    spill: Option<(PathBuf, std::fs::File)>,
}

impl BashOutput {
    fn new() -> Self {
        Self {
            tail: String::new(),
            total: 0,
            spill: None,
        }
    }

    fn push(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        self.total += text.len();
        if self.spill.is_none() && self.tail.len() + text.len() > BASH_MAX_BYTES {
            // first overflow: persist what we hold (the full prefix so far),
            // then append live to the file while RAM keeps only the tail
            let opened = write_bash_output(&self.tail).and_then(|p| {
                std::fs::OpenOptions::new()
                    .append(true)
                    .open(&p)
                    .map(|f| (p, f))
            });
            self.spill = opened.ok();
        }
        if let Some((_, f)) = &mut self.spill {
            use std::io::Write;
            let _ = f.write_all(text.as_bytes());
        }
        self.tail.push_str(text);
        if self.tail.len() > BASH_MAX_BYTES {
            self.tail = tail_bytes(&self.tail, BASH_MAX_BYTES);
        }
    }

    /// `(display_tail, truncated, full_output_path)` — same semantics the
    /// end-of-run computation had, minus the unbounded accumulation.
    fn finish(mut self) -> (String, bool, Option<PathBuf>) {
        use std::io::Write;
        let truncated = self.total > BASH_MAX_BYTES;
        let path = match self.spill.as_mut() {
            Some((p, f)) => f.flush().ok().map(|_| p.clone()),
            // spill setup failed: best effort, report the tail we have
            None if truncated => write_bash_output(&self.tail).ok(),
            None => None,
        };
        (self.tail, truncated, path)
    }
}
/// Persist full bash output to a temp file like pi's `<prefix>-<hex>.log`.
fn write_bash_output(text: &str) -> std::io::Result<PathBuf> {
    use std::io::Write;
    let path = std::env::temp_dir().join(format!(
        "{}.log",
        feather_core::provider::gen_id("feather-bash-")
    ));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)?;
    file.write_all(text.as_bytes())?;
    Ok(path)
}
struct OutputDecoder {
    pending: Vec<u8>,
}

impl OutputDecoder {
    fn new() -> Self {
        Self {
            pending: Vec::new(),
        }
    }

    fn push(&mut self, bytes: &[u8]) -> String {
        self.pending.extend_from_slice(bytes);
        match std::str::from_utf8(&self.pending) {
            Ok(text) => {
                let text = text.to_owned();
                self.pending.clear();
                text
            }
            Err(error) if error.error_len().is_none() => {
                let valid = error.valid_up_to();
                if valid == 0 {
                    return String::new();
                }
                let rest = self.pending.split_off(valid);
                let text = crate::shell::decode_output(&self.pending);
                self.pending = rest;
                text
            }
            Err(_) => {
                let text = crate::shell::decode_output(&self.pending);
                self.pending.clear();
                text
            }
        }
    }

    fn finish(&mut self) -> String {
        if self.pending.is_empty() {
            String::new()
        } else {
            let text = crate::shell::decode_output(&self.pending);
            self.pending.clear();
            text
        }
    }
}

/// Read a child pipe to the end, forwarding decoded chunks to `tx`.
async fn drain_bash<R>(mut pipe: R, tx: tokio::sync::mpsc::UnboundedSender<String>)
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    let mut decoder = OutputDecoder::new();
    let mut buf = [0u8; 8192];
    loop {
        match pipe.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let text = decoder.push(&buf[..n]).replace('\r', "");
                if !text.is_empty() && tx.send(text).is_err() {
                    return;
                }
            }
        }
    }
    let text = decoder.finish().replace('\r', "");
    if !text.is_empty() {
        let _ = tx.send(text);
    }
}
/// Append decoded text to the running output and stream it as a
/// `bash_execution_update` event.
fn append_bash_chunk(
    text: String,
    output: &mut BashOutput,
    id: &Option<Value>,
    tx: &tokio::sync::mpsc::UnboundedSender<BashMsg>,
) {
    if text.is_empty() {
        return;
    }
    output.push(&text);
    let mut event = json!({"type":"bash_execution_update","delta":text});
    if let Some(id) = id {
        event["id"] = id.clone();
    }
    let _ = tx.send(BashMsg::Update(event));
}
/// Kill the whole bash process tree. `Child::kill` only signals the direct
/// child; descendants may survive, keep the pipes open, and hold the working
/// directory locked (a sharing violation when the session is cleaned up).
async fn kill_bash_tree(child: &mut tokio::process::Child) {
    if let Some(pid) = child.id() {
        #[cfg(windows)]
        {
            let _ = feather_core::process::async_command("taskkill")
                .args(["/PID", &pid.to_string(), "/T", "/F"])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .await;
        }
        #[cfg(unix)]
        {
            // The child leads its own process group (see `process_group(0)`),
            // so a negative PID signals the whole tree.
            let _ = feather_core::process::async_command("kill")
                .args(["-9", &format!("-{pid}")])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .await;
        }
    }
    let _ = child.kill().await;
}
/// pi's `bash` RPC: run a shell command with streaming output and cancellation.
/// Returns the pi `BashResult` shape; the caller decides whether the completed
/// command becomes model context.
async fn execute_bash(
    command: String,
    cwd: PathBuf,
    id: Option<Value>,
    tx: tokio::sync::mpsc::UnboundedSender<BashMsg>,
    mut abort: tokio::sync::watch::Receiver<bool>,
) {
    if let Err(error) = crate::command_policy::check(&command) {
        let _=tx.send(BashMsg::Done(json!({"output":error,"exitCode":126,"cancelled":false,"truncated":false,"illegal":true})));
        return;
    }
    let runtime = match crate::shell::BashRuntime::discover() {
        Ok(runtime) => runtime,
        Err(error) => {
            let _ = tx.send(BashMsg::Done(json!({
                "output": error,
                "exitCode": Value::Null,
                "cancelled": false,
                "truncated": false,
            })));
            return;
        }
    };
    let mut cmd = feather_core::process::async_command(&runtime.executable);
    cmd.args(["--noprofile", "--norc", "-c", &command]);
    if let Some(path) = runtime.path {
        cmd.env("PATH", path);
    }
    cmd.env("LC_ALL", "en_US.UTF-8")
        .env("LANG", "en_US.UTF-8")
        .env_remove("BASH_ENV")
        .env_remove("ENV")
        .kill_on_drop(true);
    #[cfg(unix)]
    {
        // Lead a new process group so an abort can signal the whole tree.
        use std::os::unix::process::CommandExt;
        cmd.as_std_mut().process_group(0);
    }
    cmd.current_dir(cwd)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .stdin(std::process::Stdio::null());
    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(error) => {
            let _ = tx.send(BashMsg::Done(json!({
                "output": format!("bash: spawn failed: {error}"),
                "exitCode": Value::Null,
                "cancelled": false,
                "truncated": false,
            })));
            return;
        }
    };
    let (chunk_tx, mut chunk_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let mut readers = Vec::new();
    if let Some(pipe) = child.stdout.take() {
        readers.push(tokio::spawn(drain_bash(pipe, chunk_tx.clone())));
    }
    if let Some(pipe) = child.stderr.take() {
        readers.push(tokio::spawn(drain_bash(pipe, chunk_tx.clone())));
    }
    drop(chunk_tx);
    let mut output = BashOutput::new();
    let mut cancelled = false;
    let mut readers_done = false;
    let mut status: Option<std::process::ExitStatus> = None;
    loop {
        tokio::select! {
            chunk = chunk_rx.recv(), if !readers_done => match chunk {
                Some(text) => append_bash_chunk(text, &mut output, &id, &tx),
                None => readers_done = true,
            },
            result = child.wait(), if status.is_none() => status = result.ok(),
            changed = abort.changed() => {
                if changed.is_ok() && *abort.borrow() && !cancelled {
                    cancelled = true;
                    kill_bash_tree(&mut child).await;
                    break;
                }
            }
        }
        if readers_done && status.is_some() {
            break;
        }
    }
    // Reap both pipe readers so no handle to the working directory outlives
    // the command (Windows keeps a process's cwd locked until it exits).
    for reader in readers {
        let _ = reader.await;
    }
    while let Ok(text) = chunk_rx.try_recv() {
        append_bash_chunk(text, &mut output, &id, &tx);
    }
    // pi reports `exitCode: undefined` once a command was cancelled.
    let exit_code = if cancelled {
        Value::Null
    } else {
        status
            .as_ref()
            .and_then(std::process::ExitStatus::code)
            .map_or(Value::Null, |code| json!(code))
    };
    let (display, truncated, full_path) = output.finish();
    let mut data = json!({
        "output": display,
        "exitCode": exit_code,
        "cancelled": cancelled,
        "truncated": truncated,
    });
    if let Some(path) = full_path {
        data["fullOutputPath"] = json!(path.display().to_string());
    }
    let _ = tx.send(BashMsg::Done(data));
}

pub async fn run_with_control<R, W, F, C>(
    agent: Agent,
    reader: R,
    mut out: W,
    home: &Path,
    cwd: &Path,
    mut factory: F,
    mut control: C,
) -> anyhow::Result<()>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
    F: FnMut(Session) -> anyhow::Result<Agent>,
    C: FnMut(&mut Agent, &Value) -> Result<Value, String>,
{
    let mut events = agent.subscribe();
    let mut abort = agent.abort_sender();
    let mut queue = agent.input_queue();
    let mut cached_state = state(&agent);
    let mut cached_messages: Vec<Message> = agent.session().messages().to_vec();
    let mut idle = Some(agent);
    let mut running: Option<
        tokio::task::JoinHandle<(
            Agent,
            Result<feather_core::agent::RunResult, feather_core::agent::AgentError>,
        )>,
    > = None;
    let mut compacting: Option<tokio::task::JoinHandle<(Agent, Result<Value, String>)>> = None;
    let mut is_compacting = false;
    let mut mapper = EventMapper::default();
    let mut pending_thinking: Option<Option<ThinkingLevel>> = None;
    let mut pending_auto_compaction: Option<bool> = None;
    let mut pending_aborts = Vec::new();
    let mut bash: Option<BashRun> = None;
    let mut eof = false;
    let mut lines = reader.lines();
    loop {
        tokio::select! {
            biased;
            event = events.recv(), if running.is_some() => {
                match event {
                    Ok(event) => {
                        absorb_event(&event, &mut cached_messages);
                        for record in mapper.map(event) { emit(&mut out, &record).await?; }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => return Err(anyhow::anyhow!("RPC event stream lost {n} events")),
                    Err(_) => {}
                }
            }
            completed = async { running.as_mut().expect("guarded").await }, if running.is_some() => {
                // Drain all events before settled; join readiness can race the broadcast.
                while let Ok(event) = events.try_recv() {
                    absorb_event(&event, &mut cached_messages);
                    for record in mapper.map(event) { emit(&mut out, &record).await?; }
                }
                let (agent, result) = completed?;
                let mut agent = agent;
                if let Some(level) = pending_thinking.take() { agent.set_thinking_level(level); }
                if let Some(enabled) = pending_auto_compaction.take() { agent.set_auto_compaction(enabled); }
                running = None;
                if let Err(error) = result { emit(&mut out, &json!({"type":"error","error":error.to_string()})).await?; }
                cached_state = state(&agent);
                sync_messages(&mut cached_messages, &agent);
                idle = Some(agent);
                emit(&mut out, &json!({"type":"agent_settled"})).await?;
                is_compacting = false;
                while let Ok(event) = events.try_recv() {
                    absorb_event(&event, &mut cached_messages);
                    for record in mapper.map(event) { emit(&mut out, &record).await?; }
                }
                for command in pending_aborts.drain(..) { emit(&mut out, &response(&command, Ok(None))).await?; }
                if eof && bash.is_none() { break; }
            }
            completed = async { compacting.as_mut().expect("guarded").await }, if compacting.is_some() => {
                while let Ok(event) = events.try_recv() {
                    for record in compact_event_records(event) { emit(&mut out, &record).await?; }
                }
                let (agent, data) = completed?;
                compacting = None;
                is_compacting = false;
                cached_state = state(&agent);
                sync_messages(&mut cached_messages, &agent);
                idle = Some(agent);
                emit(&mut out, &json!({"type":"compaction_end","reason":"manual","result":data.as_ref().ok(),"aborted":data.is_err(),"willRetry":false})).await?;
                emit(&mut out, &json!({"type":"agent_settled"})).await?;
                for command in pending_aborts.drain(..) { emit(&mut out, &response(&command, Ok(None))).await?; }
                if eof && bash.is_none() { break; }
            }
            msg = async { bash.as_mut().expect("guarded").updates.recv().await }, if bash.is_some() => {
                match msg {
                    Some(BashMsg::Update(event)) => emit(&mut out, &event).await?,
                    Some(BashMsg::Done(data)) => {
                        let run = bash.take().expect("guarded");
                        if !run.exclude_from_context {
                            if let Some(agent) = idle.as_mut() {
                                let heading=if data["illegal"].as_bool().unwrap_or(false){format!("Blocked Bash request `{}` · NOT executed",run.command_text)}else{format!("Ran `{}`",run.command_text)};
                                let text=format!("{heading}\n```\n{}\n```",data["output"].as_str().unwrap_or(""));
                                let _ = agent.session_mut().push(Message::user_text(text));
                                cached_messages = agent.session().messages().to_vec();
                            }
                        }
                        emit(&mut out, &response(&run.command, Ok(Some(data)))).await?;
                        if eof && running.is_none() && compacting.is_none() { break; }
                    }
                    None => {
                        bash = None;
                        if eof && running.is_none() && compacting.is_none() { break; }
                    }
                }
            }
            line = lines.next_line(), if !eof => {
                let Some(line) = line? else {
                    eof = true;
                    if running.is_none() && compacting.is_none() && bash.is_none() { break; }
                    let _ = abort.send(true);
                    if let Some(run) = bash.as_ref() { let _ = run.abort.send(true); }
                    continue;
                };
                let command = match serde_json::from_str::<Value>(&line) {
                    Ok(value) if value.is_object() => value,
                    _ => { emit(&mut out, &response(&json!({"type":"parse"}), Err("Failed to parse command: expected a JSON object".into()))).await?; continue; }
                };
                if command.get("id").is_some_and(|id| !id.is_string()) {
                    emit(&mut out, &response(&command, Err("id must be a string".into()))).await?;
                    continue;
                }
                let kind = command["type"].as_str().unwrap_or("");
                let result: Result<Option<Value>, String> = match kind {
                    "prompt" | "steer" | "follow_up" => match user_message(&command) {
                        Err(e) => Err(e),
                        Ok(message) => {
                            if kind != "prompt" || running.is_some() || compacting.is_some() {
                                let behavior = if kind == "steer" { "steer" } else if kind == "follow_up" { "followUp" } else { command["streamingBehavior"].as_str().unwrap_or("") };
                                if !["steer", "followUp"].contains(&behavior) { Err("Agent is streaming; specify streamingBehavior: steer or followUp".into()) }
                                else {
                                    queue.push(message, behavior == "steer");
                                    Ok(Some(json!({"disposition":"queued"})))
                                }
                            } else {
                                emit(&mut out, &response(&command, Ok(Some(json!({"disposition":"started"}))))).await?;
                                emit(&mut out, &json!({"type":"agent_start"})).await?;
                                let wire = wire_message(&message);
                                emit(&mut out, &json!({"type":"message_start","message":wire})).await?;
                                emit(&mut out, &json!({"type":"message_end","message":wire})).await?;
                                if let Some(a) = idle.as_ref() {
                                    sync_messages(&mut cached_messages, a);
                                }
                                cached_messages.push(message.clone());
                                mapper = EventMapper::default();
                                let mut agent = idle.take().expect("idle");
                                agent.reset_abort();
                                abort = agent.abort_sender();
                                events = agent.subscribe();
                                running = Some(tokio::spawn(async move { let result = agent.prompt_message_prepared(message).await; (agent, result) }));
                                continue;
                            }
                        }
                    },
                    "abort" => {
                        if running.is_some() || compacting.is_some() { let _ = abort.send(true); pending_aborts.push(command); continue; }
                        Ok(None)
                    }
                    "clear_queue" => {
                        let (steering, follow) = queue.clear();
                        Ok(Some(json!({"steering":steering,"followUp":follow})))
                    }
                    "bash" => {
                        if bash.is_some() {
                            Err("A bash command is already running".into())
                        } else {
                            match command["command"].as_str() {
                                Some(text) if !text.trim().is_empty() => {
                                    let exclude =
                                        command["excludeFromContext"].as_bool().unwrap_or(false);
                                    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
                                    let (bash_abort, abort_rx) = tokio::sync::watch::channel(false);
                                    let id = command.get("id").cloned();
                                    let cwd_owned = cwd.to_path_buf();
                                    let task_text = text.to_string();
                                    tokio::spawn(async move {
                                        execute_bash(task_text, cwd_owned, id, tx, abort_rx).await;
                                    });
                                    bash = Some(BashRun {
                                        command: command.clone(),
                                        command_text: text.to_string(),
                                        exclude_from_context: exclude,
                                        abort: bash_abort,
                                        updates: rx,
                                    });
                                    continue;
                                }
                                _ => Err("bash: command is required".into()),
                            }
                        }
                    }
                    "abort_bash" => {
                        if let Some(run) = bash.as_ref() {
                            let _ = run.abort.send(true);
                        }
                        Ok(None)
                    }
                    "set_session_name" => {
                        let name = command["name"].as_str().unwrap_or("").trim().to_string();
                        if name.is_empty() {
                            Err("Session name cannot be empty".into())
                        } else {
                            let dir = session_dir(idle.as_ref(), &cached_state);
                            match write_session_name(&dir, &name) {
                                Ok(()) => {
                                    cached_state["sessionName"] = json!(name);
                                    Ok(None)
                                }
                                Err(error) => Err(error.to_string()),
                            }
                        }
                    }
                    "get_entries" => {
                        let dir = session_dir(idle.as_ref(), &cached_state);
                        read_transcript(&dir).and_then(|entries| {
                            let start = match command.get("since").and_then(Value::as_str) {
                                Some(since) => entries
                                    .iter()
                                    .position(|entry| entry.id == since)
                                    .map(|index| index + 1)
                                    .ok_or_else(|| format!("Entry not found: {since}"))?,
                                None => 0,
                            };
                            let leaf_id = entries.last().map(|entry| entry.id.clone());
                            let payload = entries_payload(&entries, start);
                            Ok(Some(json!({"entries": payload, "leafId": leaf_id})))
                        })
                    }
                    "get_fork_messages" => {
                        let dir = session_dir(idle.as_ref(), &cached_state);
                        read_transcript(&dir).map(|entries| {
                            let messages: Vec<Value> = entries
                                .iter()
                                .filter(|entry| entry.message.role == Role::User)
                                .map(|entry| {
                                    json!({"entryId": entry.id, "text": entry.message.text()})
                                })
                                .collect();
                            Some(json!({"messages": messages}))
                        })
                    }
                    "set_steering_mode" | "set_follow_up_mode" => {
                        match command["mode"].as_str() {
                            Some("all" | "one-at-a-time") => { queue.set_mode(kind == "set_steering_mode", command["mode"] == "all"); Ok(None) }
                            _ => Err("mode must be all or one-at-a-time".into())
                        }
                    }
                    "get_state" => {
                        let mut v = idle.as_ref().map(state).unwrap_or_else(|| cached_state.clone());
                        let (steering, follow) = queue.snapshot();
                        v["isStreaming"] = json!(running.is_some());
                        v["isCompacting"] = json!(is_compacting);
                        v["pendingMessageCount"] = json!(steering.len()+follow.len());
                        if let Some(agent) = idle.as_ref() {
                            v["thinkingLevel"] = json!(agent.thinking_level().map(|l| clamp_level(agent,l).as_str()).unwrap_or("off"));
                        }
                        Ok(Some(v))
                    }
                    "get_available_models" => {
                        let global_context = idle.as_ref().map(|a| a.config().context_window).unwrap_or(200_000);
                        let models = models::load().map(|cat| cat.providers.iter().flat_map(|(pid,p)| p.models.iter().map(move |m| model_object(pid,p,m,global_context))).collect::<Vec<_>>()).unwrap_or_default();
                        Ok(Some(json!({"models":models})))
                    }
                    "get_available_thinking_levels" => {
                        let levels = match idle.as_ref() {
                            Some(agent) => available_thinking_levels(agent),
                            None => levels_for_model(&cached_state["model"]),
                        };
                        Ok(Some(json!({"levels": levels})))
                    }
                    "get_commands" => Ok(Some(commands_object(home, cwd))),
                    "get_messages" => {
                        let messages: Vec<Value> = match idle.as_ref() {
                            Some(agent) => agent.session().messages().iter().map(wire_message).collect(),
                            None => cached_messages.iter().map(wire_message).collect(),
                        };
                        Ok(Some(json!({"messages": messages})))
                    }
                    "get_last_assistant_text" => {
                        let text = match idle.as_ref() {
                            Some(agent) => agent.session().messages().iter().rev().find(|m| m.role == Role::Assistant && !m.text().is_empty()).map(Message::text),
                            None => cached_messages.iter().rev().find(|m| m.role == Role::Assistant && !m.text().is_empty()).map(Message::text),
                        };
                        Ok(Some(json!({"text": text})))
                    }
                    "get_session_stats" => {
                        let stats = match idle.as_ref() {
                            Some(agent) => session_stats(agent),
                            None => cached_stats(&cached_state, &cached_messages),
                        };
                        Ok(Some(stats))
                    }
                    "set_thinking_level" if running.is_some() || compacting.is_some() => match command["level"].as_str().and_then(ThinkingLevel::parse) {
                        Some(level) => {
                            pending_thinking = Some(if level == ThinkingLevel::Off { None } else { Some(level) });
                            cached_state["thinkingLevel"] = json!(level.as_str());
                            Ok(None)
                        }
                        None => Err("Invalid thinking level".into()),
                    },
                    "set_auto_compaction" if running.is_some() || compacting.is_some() => match command["enabled"].as_bool() {
                        Some(enabled) => {
                            pending_auto_compaction = Some(enabled);
                            cached_state["autoCompactionEnabled"] = json!(enabled);
                            Ok(None)
                        }
                        None => Err("enabled must be boolean".into()),
                    },
                    _ if running.is_some() || compacting.is_some() => Err("Command requires an idle session".into()),
                    "set_thinking_level" => match command["level"].as_str().and_then(ThinkingLevel::parse) {
                        Some(level) => {
                            let agent = idle.as_mut().unwrap();
                            match crate::config::Settings::load().save_thinking_level(&agent.model_id(), level.as_str()) {
                                Ok(()) => { let level = clamp_level(agent, level); agent.set_thinking_level(if level == ThinkingLevel::Off { None } else { Some(level) }); Ok(None) }
                                Err(e) => Err(e.to_string()),
                            }
                        }
                        None => Err("Invalid thinking level".into()),
                    },
                    "cycle_thinking_level" => {
                        let agent = idle.as_mut().unwrap();
                        let levels = available_thinking_levels(agent);
                        if levels == vec!["off".to_string()] { Ok(None) } else {
                            let cur = agent.thinking_level().map(|l| l.as_str()).unwrap_or("off");
                            let idx = levels.iter().position(|l| *l == cur).unwrap_or(0);
                            let next = &levels[(idx + 1) % levels.len()];
                            let level = ThinkingLevel::parse(next.as_str()).unwrap_or(ThinkingLevel::Off);
                            agent.set_thinking_level(if level == ThinkingLevel::Off { None } else { Some(level) });
                            Ok(Some(json!({"level":next})))
                        }
                    }
                    "export_html" => export_html(idle.as_ref().unwrap(), command["outputPath"].as_str()).map(|p| Some(json!({"path":p}))),
                    "set_model" | "cycle_model" => {
                        let agent = idle.as_mut().unwrap();
                        let result = control(agent, &command);
                        if result.is_ok() { cached_state = state(agent); }
                        result.map(Some)
                    }
                    "set_auto_compaction" => match command["enabled"].as_bool() {
                        Some(enabled) => { idle.as_mut().unwrap().set_auto_compaction(enabled); Ok(None) }
                        None => Err("enabled must be boolean".into()),
                    },
                    "compact" => {
                        emit(&mut out, &json!({"type":"compaction_start","reason":"manual"})).await?;
                        if let Some(a) = idle.as_ref() {
                            sync_messages(&mut cached_messages, a);
                        }
                        let mut agent = idle.take().unwrap();
                        agent.reset_abort();
                        abort = agent.abort_sender();
                        events = agent.subscribe();
                        is_compacting = true;
                        compacting = Some(tokio::spawn(async move {
                            let before = feather_core::tokenizer::estimate_context_tokens(agent.session().messages());
                            let result = match agent.compact("manual").await {
                                Ok(_) => {
                                    let after = feather_core::tokenizer::estimate_context_tokens(agent.session().messages());
                                    let summary = agent.session().messages().first().map(Message::text).unwrap_or_default();
                                    Ok(json!({"summary":summary,"tokensBefore":before,"estimatedTokensAfter":after}))
                                }
                                Err(e) => Err(e.to_string()),
                            };
                            (agent, result)
                        }));
                        Ok(Some(json!({"disposition":"started"})))
                    }
                    "abort_compaction" => {
                        if compacting.is_some() { let _ = abort.send(true); pending_aborts.push(command); continue; }
                        Ok(None)
                    },
                    "new_session" | "switch_session" | "clone" => {
                        let current = idle.as_ref().unwrap();
                        let next = match kind {
                            "new_session" => Session::new(home, cwd),
                            "clone" => current.session().fork(),
                            _ => {
                                let path = command["sessionPath"].as_str().unwrap_or("");
                                let path = Path::new(path);
                                let dir = if path.is_file() { path.parent().unwrap_or(path) } else { path };
                                if dir == current.session().dir() {
                                    emit(&mut out, &response(&command, Ok(Some(json!({"cancelled":false}))))).await?;
                                    continue;
                                }
                                Session::open(dir, home)
                            }
                        };
                        match next.map_err(anyhow::Error::from).and_then(&mut factory) {
                            Ok(agent) => {
                                events = agent.subscribe(); abort = agent.abort_sender(); queue = agent.input_queue();
                                cached_state = state(&agent); cached_messages = agent.session().messages().to_vec(); idle = Some(agent);
                                Ok(Some(json!({"cancelled":false})))
                            }
                            Err(e) => Err(e.to_string()),
                        }
                    }
                    _ => Err(format!("Unsupported command: {kind}")),
                };
                emit(&mut out, &response(&command, result)).await?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn bash_output_rolls_tail_and_spills_once_cap_exceeded() {
        let mut out = BashOutput::new();
        let chunk = "x".repeat(10_000);
        for _ in 0..8 {
            out.push(&chunk); // 80 KB total, cap is 50 KB
        }
        let (tail, truncated, path) = out.finish();
        assert!(truncated, "80 KB output must be marked truncated");
        assert_eq!(tail.len(), 50 * 1024, "RAM keeps only the last 50 KB");
        let spilled = path.expect("full output spilled to a temp file");
        let full = std::fs::read_to_string(&spilled).unwrap();
        assert_eq!(full.len(), 80_000, "spill file holds the full output");
        let _ = std::fs::remove_file(&spilled);

        // under the cap: no spill, no truncation, output round-trips
        let mut small = BashOutput::new();
        small.push("hello");
        let (tail, truncated, path) = small.finish();
        assert!(!truncated && path.is_none());
        assert_eq!(tail, "hello");
    }

    use super::*;
    #[test]
    fn responses_and_images_follow_pi_wire_names() {
        let r = response(
            &json!({"id":"req-1","type":"get_state"}),
            Ok(Some(json!({"isStreaming":false}))),
        );
        assert_eq!(r["type"], "response");
        assert_eq!(r["command"], "get_state");
        assert_eq!(r["id"], "req-1");
        let m = user_message(&json!({"message":"hi\u{2028}there","images":[{"type":"image","mimeType":"image/png","data":"AAAA"}]})).unwrap();
        let wire = wire_message(&m);
        assert_eq!(wire["content"][1]["mimeType"], "image/png");
        assert_eq!(m.text(), "hi\u{2028}there");
        assert!(user_message(&json!({"text":"not pi's field"})).is_err());
    }

    #[test]
    fn pricing_is_per_million_and_wire_usage_always_has_cost() {
        let cost = models::ModelCost {
            input: Some(3.0),
            output: Some(15.0),
            cache_read: Some(0.3),
            cache_write: Some(3.75),
        };
        let u = Usage {
            input: 1_000_000,
            output: 2_000_000,
            cache_read: 0,
            cache_write: 0,
        };
        let priced = usage_cost(u, Some(&cost));
        assert_eq!(priced["input"], 3.0);
        assert_eq!(priced["output"], 30.0);
        assert_eq!(priced["total"], 33.0);

        let zeros = wire_usage(Usage::default());
        assert!(
            zeros["cost"].is_object(),
            "cost is always present on the wire"
        );
        assert_eq!(zeros["cost"]["total"], 0.0);

        // assistant wire messages price from the message's own model field
        let m = Message::assistant(
            vec![ContentBlock::Text { text: "x".into() }],
            "unknown/model",
            Api::Faux,
            Usage {
                input: 1_000_000,
                output: 0,
                cache_read: 0,
                cache_write: 0,
            },
            StopReason::Stop,
        );
        let wire = wire_message(&m);
        assert_eq!(wire["model"], "unknown/model");
        assert!(wire["usage"]["cost"].is_object());
    }

    #[test]
    fn session_stats_counts_tool_calls() {
        let call = |id: &str, name: &str| {
            ContentBlock::ToolCall(feather_core::message::ToolCall {
                id: id.into(),
                name: name.into(),
                arguments: json!({}),
            })
        };
        let msgs = vec![
            Message::user_text("hi"),
            Message::assistant(
                vec![call("c1", "read")],
                "m",
                Api::Faux,
                Usage {
                    input: 10,
                    output: 5,
                    cache_read: 0,
                    cache_write: 0,
                },
                StopReason::ToolUse,
            ),
            Message::tool_result("c1", "read", "ok", false),
            Message::assistant(
                vec![
                    ContentBlock::Text {
                        text: "done".into(),
                    },
                    call("c2", "bash"),
                ],
                "m",
                Api::Faux,
                Usage::default(),
                StopReason::ToolUse,
            ),
        ];
        let stats = compute_stats(&msgs, "/tmp/session.jsonl".into(), "sid", 1_000);
        assert_eq!(stats["userMessages"], 1);
        assert_eq!(stats["assistantMessages"], 2);
        assert_eq!(stats["toolCalls"], 2);
        assert_eq!(stats["toolResults"], 1);
        assert_eq!(stats["totalMessages"], 4);
        assert_eq!(stats["tokens"]["input"], 10);
        assert_eq!(stats["contextUsage"]["contextWindow"], 1_000);
    }

    #[test]
    fn commands_object_discovers_prompts_and_skills() {
        let base = std::env::temp_dir().join(feather_core::provider::gen_id("rpc-commands"));
        let home = base.join("home");
        let cwd = base.join("cwd");
        std::fs::create_dir_all(home.join("prompts")).unwrap();
        std::fs::write(
            home.join("prompts").join("fix.md"),
            "---\ndescription: Fix failing tests\n---\nbody\n",
        )
        .unwrap();
        std::fs::create_dir_all(cwd.join(".feather").join("skills").join("demo")).unwrap();
        std::fs::write(
            cwd.join(".feather")
                .join("skills")
                .join("demo")
                .join("SKILL.md"),
            "---\nname: demo\ndescription: Demo skill\n---\nbody\n",
        )
        .unwrap();

        let value = commands_object(&home, &cwd);
        let commands = value["commands"].as_array().unwrap();
        let prompt = commands
            .iter()
            .find(|c| c["name"] == "fix")
            .expect("prompt command");
        assert_eq!(prompt["source"], "prompt");
        assert_eq!(prompt["description"], "Fix failing tests");
        assert_eq!(prompt["sourceInfo"]["scope"], "user");
        let skill = commands
            .iter()
            .find(|c| c["name"] == "skill:demo")
            .expect("skill command");
        assert_eq!(skill["source"], "skill");
        assert_eq!(skill["sourceInfo"]["scope"], "project");

        std::fs::remove_dir_all(&base).unwrap();
    }
}
