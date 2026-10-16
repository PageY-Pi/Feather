//! Regression: thinking blocks are not sent to OpenAI-compatible providers, so
//! they must not count against the context window.
//!
//! `volcengine-plan` (and every other `openai-completions` provider) serializes
//! an assistant message with `Message::text()` plus tool calls only — reasoning
//! never reaches the wire. The token estimate that decides whether to compact,
//! and that verifies compaction afterwards, counted reasoning anyway. A session
//! whose real request is far under the limit therefore looked oversized, and
//! when the cut planner (correctly, reasoning-free) found nothing to summarize,
//! the run died with "context remains over model limit after compaction".
use feather_core::{
    agent::{AgentConfig, ProviderRef},
    compaction::CompactionSettings,
    message::{Api, ContentBlock, Message, StopReason, ToolCall, Usage},
    provider::faux::{FauxProvider, FauxScript},
    tokenizer,
    tool::ToolRegistry,
    Agent, Session,
};
use std::path::Path;
use std::sync::Arc;

/// 272_000 window with 16_384 reserve: the user's configuration, where the
/// failure surfaced as "277548 >= 272000".
const WINDOW: u64 = 272_000;
const RESERVE: u64 = 16_384;

fn reasoning_only_session(id: &str, thinking_chars: usize, text_chars: usize) -> Session {
    let mut session = Session::memory(Path::new("."), id);
    session.push(Message::user_text("first question")).unwrap();
    // An assistant turn whose visible text is small but whose reasoning is huge.
    session
        .push(Message::assistant(
            vec![
                ContentBlock::Thinking {
                    text: "x".repeat(thinking_chars),
                    signature: None,
                },
                ContentBlock::Text {
                    text: "y".repeat(text_chars),
                },
            ],
            "deepseek-v4.1-flash",
            Api::OpenaiCompletions,
            Usage::default(),
            StopReason::Stop,
        ))
        .unwrap();
    session.push(Message::user_text("latest question")).unwrap();
    session
}

fn config() -> AgentConfig {
    AgentConfig {
        model: "deepseek-v4.1-flash".into(),
        api: Api::OpenaiCompletions,
        system_prompt: "test".into(),
        max_tokens: 1024,
        temperature: None,
        context_window: WINDOW,
        thinking_level: None,
        compaction: CompactionSettings {
            reserve_tokens: RESERVE,
            keep_recent_tokens: 20_000,
            ..Default::default()
        },
        compaction_model: None,
        tools: vec![].into(),
    }
}

/// The wire payload for an openai-completions request carries no reasoning.
#[test]
fn openai_completions_request_body_omits_reasoning() {
    let session = reasoning_only_session("wire", 400_000, 40);
    // `convert_messages` is exactly what the provider serializes into the body.
    let body = feather_core::provider::openai::OpenAiProvider::convert_messages(session.messages());
    let serialized = serde_json::Value::Array(body).to_string();
    assert!(
        !serialized.contains(&"x".repeat(64)),
        "reasoning text must never be sent to an openai-completions endpoint"
    );
    assert!(
        serialized.contains(&"y".repeat(32)),
        "visible text must still be sent"
    );
}

/// The estimate used for the compaction decision must match that wire payload.
#[test]
fn estimate_for_openai_completions_excludes_reasoning() {
    let session = reasoning_only_session("estimate", 2_000_000, 400);
    let messages = session.messages();

    let naive =
        tokenizer::estimate_request_input_tokens(Api::AnthropicMessages, "test", messages, &[]);
    let sent =
        tokenizer::estimate_request_input_tokens(Api::OpenaiCompletions, "test", messages, &[]);

    assert!(
        naive > sent,
        "counting reasoning ({naive}) must exceed the wire-accurate estimate ({sent})"
    );
    assert!(
        naive > WINDOW - RESERVE,
        "the inflated estimate is what triggered compaction: {naive}"
    );
    assert!(
        sent < WINDOW - RESERVE,
        "the real request is comfortably under the threshold: {sent}"
    );
}

/// The user-visible failure: a reasoning-heavy session must not compact at all,
/// and must certainly not fail the turn.
#[tokio::test]
async fn reasoning_heavy_session_does_not_compaction_fail() {
    let session = reasoning_only_session("no-fail", 2_000_000, 400);
    let mut agent = Agent::new(
        config(),
        session,
        Arc::new(ToolRegistry::new()),
        ProviderRef::Faux(FauxProvider::new(FauxScript::new().with_text("ANSWER"))),
        None,
    );
    let mut events = agent.subscribe();
    let result = agent
        .prompt("latest question")
        .await
        .unwrap_or_else(|e| panic!("a session whose real request fits must not fail: {e}"));
    assert_eq!(result.final_text, "ANSWER");
    let mut seen = Vec::new();
    while let Ok(event) = events.try_recv() {
        seen.push(event);
    }
    assert!(
        !seen
            .iter()
            .any(|e| matches!(e, feather_core::agent::AgentEvent::CompactionDone { .. })),
        "reasoning that is never sent must not trigger compaction"
    );
}

/// Reasoning still counts where the provider does send it: anthropic replays
/// thinking blocks in the request history.
#[test]
fn estimate_for_anthropic_still_counts_reasoning() {
    let session = reasoning_only_session("anthropic", 2_000_000, 400);
    let messages = session.messages();
    let sent =
        tokenizer::estimate_request_input_tokens(Api::AnthropicMessages, "test", messages, &[]);
    assert!(
        sent > WINDOW - RESERVE,
        "anthropic sends reasoning, so it must count: {sent}"
    );
}

/// Tool-call arguments are sent and must keep counting for every api.
#[test]
fn tool_arguments_count_for_openai_completions() {
    let mut session = Session::memory(Path::new("."), "tools");
    session.push(Message::user_text("q")).unwrap();
    session
        .push(Message::assistant(
            vec![ContentBlock::ToolCall(ToolCall {
                id: "call-1".into(),
                name: "read".into(),
                arguments: serde_json::json!({ "path": "a".repeat(200_000) }),
            })],
            "deepseek-v4.1-flash",
            Api::OpenaiCompletions,
            Usage::default(),
            StopReason::Stop,
        ))
        .unwrap();
    let session = { session };
    let sent = tokenizer::estimate_request_input_tokens(
        Api::OpenaiCompletions,
        "",
        session.messages(),
        &[],
    );
    assert!(
        sent > 10_000,
        "tool arguments are on the wire and must count, got {sent}"
    );
}
