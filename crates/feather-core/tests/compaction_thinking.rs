//! Regression tests for the real HTTP request bodies used by compaction.
//!
//! Compaction is maintenance work: when a reasoning-capable provider is active it
//! should summarize with a cheap `low` effort, independent of the live turn's
//! thinking level, and it must omit reasoning knobs for explicit non-reasoning
//! metadata.

use feather_core::{
    agent::{AgentConfig, AgentEvent, ProviderRef},
    compaction::CompactionSettings,
    message::{Api, ContentBlock, Message, StopReason, Usage},
    provider::{
        openai::OpenAiProvider, pi::PiProvider, responses::ResponsesProvider, ThinkingLevel,
    },
    tool::ToolRegistry,
    Agent, Session,
};
use serde_json::{json, Value};
use std::{
    io::{Read, Write},
    net::TcpListener,
    path::Path,
    sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc, Arc,
    },
    time::{Duration, Instant},
};

static SESSION_COUNTER: AtomicUsize = AtomicUsize::new(0);

#[derive(Debug)]
struct Capture {
    path: String,
    body: Value,
}

fn answer(text: &str) -> Message {
    Message::assistant(
        vec![ContentBlock::Text { text: text.into() }],
        "seeded",
        Api::OpenaiCompletions,
        Usage::default(),
        StopReason::Stop,
    )
}

fn seeded() -> Session {
    let id = format!(
        "compaction-thinking-{}-{}",
        std::process::id(),
        SESSION_COUNTER.fetch_add(1, Ordering::Relaxed)
    );
    let mut session = Session::memory(Path::new("."), id);
    session.push(Message::user_text("old question")).unwrap();
    session.push(answer(&"old answer ".repeat(2000))).unwrap();
    session.push(Message::user_text("recent question")).unwrap();
    session.push(answer("PRIOR FINAL PURE TEXT")).unwrap();
    session
}

fn config(api: Api, thinking: Option<ThinkingLevel>, window: u64, keep: u64) -> AgentConfig {
    AgentConfig {
        model: "test-model".into(),
        api,
        system_prompt: "test".into(),
        max_tokens: 1024,
        temperature: None,
        context_window: window,
        thinking_level: thinking,
        compaction: CompactionSettings {
            reserve_tokens: 100,
            keep_recent_tokens: keep,
            ..Default::default()
        },
        compaction_model: None,
        tools: vec![].into(),
    }
}

fn agent(
    provider: ProviderRef,
    api: Api,
    thinking: Option<ThinkingLevel>,
    window: u64,
    keep: u64,
) -> Agent {
    Agent::new(
        config(api, thinking, window, keep),
        seeded(),
        Arc::new(ToolRegistry::new()),
        provider,
        None,
    )
}

fn openai_agent(
    base: &str,
    compat: Value,
    thinking: Option<ThinkingLevel>,
    window: u64,
    keep: u64,
) -> Agent {
    let provider = OpenAiProvider::new("unused", Some(base.into())).with_compat(compat);
    agent(
        ProviderRef::OpenAi(Arc::new(provider)),
        Api::OpenaiCompletions,
        thinking,
        window,
        keep,
    )
}

fn responses_agent(base: &str, compat: Value, thinking: Option<ThinkingLevel>) -> Agent {
    let provider = ResponsesProvider::new("unused", Some(base.into())).with_compat(compat);
    agent(
        ProviderRef::Responses(Arc::new(provider)),
        Api::OpenaiResponses,
        thinking,
        100_000,
        1,
    )
}

fn pi_agent(base: &str, compat: Value, thinking: Option<ThinkingLevel>) -> Agent {
    let provider = PiProvider::new("unused", Some(base.into())).with_compat(compat);
    agent(
        ProviderRef::Pi(Arc::new(provider)),
        Api::PiMessages,
        thinking,
        100_000,
        1,
    )
}

fn full_level_map() -> Value {
    json!({
        "off": "off",
        "minimal": null,
        "low": "low",
        "medium": "medium",
        "high": "high",
        "xhigh": "xhigh",
        "max": "max",
    })
}

fn reasoning_compat() -> Value {
    json!({
        "reasoning": true,
        "supportsReasoningEffort": true,
        "thinkingLevelMap": full_level_map(),
    })
}

fn start_server(replies: Vec<&'static str>) -> (String, mpsc::Receiver<Capture>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let addr = listener.local_addr().unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for reply in replies {
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(e)
                        if e.kind() == std::io::ErrorKind::WouldBlock
                            && Instant::now() < deadline =>
                    {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => return,
                }
            };
            let raw = read_http_request(&mut stream).unwrap_or_default();
            let path = request_path(&raw);
            let body = request_body(&raw);
            let _ = tx.send(Capture {
                path: path.clone(),
                body,
            });
            let sse = sse_for(&path, reply);
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                sse.len(), sse
            );
            let _ = stream.write_all(response.as_bytes());
        }
    });
    (format!("http://{}", addr), rx)
}

fn read_http_request(stream: &mut std::net::TcpStream) -> Option<String> {
    stream.set_read_timeout(Some(Duration::from_secs(3))).ok()?;
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    loop {
        let n = stream.read(&mut tmp).ok()?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
        let raw = String::from_utf8_lossy(&buf);
        if let Some(split) = raw.find("\r\n\r\n") {
            let len = raw[..split]
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length: ")
                        .and_then(|v| v.trim().parse::<usize>().ok())
                })
                .unwrap_or(0);
            if buf.len() >= split + 4 + len {
                break;
            }
        }
    }
    Some(String::from_utf8_lossy(&buf).to_string())
}

fn request_path(raw: &str) -> String {
    raw.lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or_default()
        .to_string()
}

fn request_body(raw: &str) -> Value {
    raw.find("\r\n\r\n")
        .and_then(|split| serde_json::from_str(&raw[split + 4..]).ok())
        .unwrap_or(Value::Null)
}

fn sse_for(path: &str, text: &str) -> String {
    if path.contains("/chat/completions") {
        let delta = json!({"choices":[{"delta":{"content":text}}]}).to_string();
        let done = json!({
            "choices":[{"delta":{},"finish_reason":"stop"}],
            "usage":{"prompt_tokens":1,"completion_tokens":1}
        })
        .to_string();
        format!("data: {delta}\n\ndata: {done}\n\ndata: [DONE]\n\n")
    } else if path.contains("/messages") {
        let delta = json!({"type":"text_delta","contentIndex":0,"delta":text}).to_string();
        let done = json!({
            "type":"done",
            "reason":"stop",
            "usage":{"input":1,"output":1,"cacheRead":0,"cacheWrite":0}
        })
        .to_string();
        format!("data: {delta}\n\ndata: {done}\n\n")
    } else {
        let delta =
            json!({"type":"response.output_text.delta","output_index":0,"delta":text}).to_string();
        let done = json!({
            "type":"response.completed",
            "response":{"status":"completed","usage":{"input_tokens":1,"output_tokens":1}}
        })
        .to_string();
        format!("data: {delta}\n\ndata: {done}\n\n")
    }
}

fn recv_capture(rx: &mpsc::Receiver<Capture>) -> Capture {
    rx.recv_timeout(Duration::from_secs(5)).unwrap()
}

async fn compact_body(mut agent: Agent, rx: mpsc::Receiver<Capture>) -> Value {
    let saved = tokio::time::timeout(Duration::from_secs(5), agent.compact("manual"))
        .await
        .unwrap()
        .unwrap();
    assert!(saved > 0, "seeded history should compact");
    recv_capture(&rx).body
}

async fn openai_compact_body(compat: Value, thinking: Option<ThinkingLevel>) -> Value {
    let (base, rx) = start_server(vec!["SHORT SUMMARY"]);
    compact_body(openai_agent(&base, compat, thinking, 100_000, 1), rx).await
}

fn no_openai_reasoning(body: &Value) {
    assert!(body.get("reasoning_effort").is_none(), "{body}");
    assert!(body.get("reasoning").is_none(), "{body}");
    assert!(body.get("thinking").is_none(), "{body}");
    assert!(body.get("enable_thinking").is_none(), "{body}");
}

fn drain(events: &mut tokio::sync::broadcast::Receiver<AgentEvent>) -> Vec<AgentEvent> {
    let mut out = vec![];
    while let Ok(event) = events.try_recv() {
        out.push(event);
    }
    out
}

#[tokio::test]
async fn completions_manual_compaction_uses_low_for_reasoning_metadata_regardless_active_level() {
    for thinking in [
        None,
        Some(ThinkingLevel::Off),
        Some(ThinkingLevel::High),
        Some(ThinkingLevel::Max),
    ] {
        let body = openai_compact_body(reasoning_compat(), thinking).await;
        assert_eq!(
            body["reasoning_effort"],
            json!("low"),
            "{thinking:?}: {body}"
        );
    }
}

#[tokio::test]
async fn automatic_compaction_is_low_then_live_turn_keeps_high_and_summary_is_private() {
    let (base, rx) = start_server(vec!["SHORT SUMMARY", "NEW FINAL PURE TEXT"]);
    let mut agent = openai_agent(
        &base,
        reasoning_compat(),
        Some(ThinkingLevel::High),
        1000,
        1,
    );
    let mut events = agent.subscribe();

    let result = tokio::time::timeout(Duration::from_secs(5), agent.prompt("next question"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result.final_text, "NEW FINAL PURE TEXT");

    let first = recv_capture(&rx);
    let second = recv_capture(&rx);
    assert!(first.path.contains("/chat/completions"));
    assert_eq!(
        first.body["reasoning_effort"],
        json!("low"),
        "summary request: {}",
        first.body
    );
    assert_eq!(
        second.body["reasoning_effort"],
        json!("high"),
        "live request: {}",
        second.body
    );

    let events = drain(&mut events);
    let deltas: Vec<_> = events
        .iter()
        .filter_map(|e| {
            if let AgentEvent::TextDelta(t) = e {
                Some(t.as_str())
            } else {
                None
            }
        })
        .collect();
    assert_eq!(deltas, vec!["NEW FINAL PURE TEXT"]);
}

#[tokio::test]
async fn explicit_reasoning_false_omits_reasoning_params_for_all_http_apis() {
    let compat = json!({
        "reasoning": false,
        "supportsReasoningEffort": true,
        "thinkingLevelMap": full_level_map(),
    });

    let body = openai_compact_body(compat.clone(), Some(ThinkingLevel::High)).await;
    no_openai_reasoning(&body);

    let (base, rx) = start_server(vec!["SHORT SUMMARY"]);
    let body = compact_body(
        responses_agent(&base, compat.clone(), Some(ThinkingLevel::High)),
        rx,
    )
    .await;
    assert!(body.get("reasoning").is_none(), "{body}");

    let (base, rx) = start_server(vec!["SHORT SUMMARY"]);
    let body = compact_body(pi_agent(&base, compat, Some(ThinkingLevel::High)), rx).await;
    assert!(body["options"].get("reasoning").is_none(), "{body}");
}

#[tokio::test]
async fn responses_and_pi_reasoning_true_compaction_use_low() {
    let compat = reasoning_compat();

    let (base, rx) = start_server(vec!["SHORT SUMMARY"]);
    let body = compact_body(
        responses_agent(&base, compat.clone(), Some(ThinkingLevel::Max)),
        rx,
    )
    .await;
    assert_eq!(body["reasoning"]["effort"], json!("low"), "{body}");

    let (base, rx) = start_server(vec!["SHORT SUMMARY"]);
    let body = compact_body(pi_agent(&base, compat, Some(ThinkingLevel::Max)), rx).await;
    assert_eq!(body["options"]["reasoning"], json!("low"), "{body}");
}

#[tokio::test]
async fn custom_map_clamps_compaction_low_to_high() {
    let compat = json!({
        "reasoning": true,
        "supportsReasoningEffort": true,
        "thinkingLevelMap": { "low": null, "medium": null, "high": "high" },
    });
    let body = openai_compact_body(compat, Some(ThinkingLevel::High)).await;
    assert_eq!(body["reasoning_effort"], json!("high"), "{body}");
}

#[tokio::test]
async fn completions_without_metadata_uses_low_only_when_active_thinking_is_some() {
    let body = openai_compact_body(Value::Null, Some(ThinkingLevel::High)).await;
    assert_eq!(
        body["reasoning_effort"],
        json!("low"),
        "active thinking should imply reasoning: {body}"
    );

    let body = openai_compact_body(Value::Null, None).await;
    no_openai_reasoning(&body);
}
