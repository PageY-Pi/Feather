//! End-to-end compaction test: a seeded over-budget context must be
//! summarized through a real provider round-trip (faux), the linear context
//! rewritten (summary + shrunk kept span), recent turns preserved, and the
//! raw transcript untouched.

use feather_core::agent::{Agent, AgentConfig, AgentEvent, ProviderRef};
use feather_core::compaction::CompactionSettings;
use feather_core::message::{Api, ContentBlock, Message, ToolCall, Usage};
use feather_core::provider::faux::{FauxProvider, FauxScript};
use feather_core::session::Session;
use std::path::PathBuf;
use std::sync::Arc;

fn temp_root(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "feather-compact-{tag}-{}-{}",
        std::process::id(),
        feather_core::message::now_millis()
    ));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn asst_write(path: &str) -> Message {
    Message::assistant(
        vec![ContentBlock::ToolCall(ToolCall {
            id: format!("call-{path}"),
            name: "write".into(),
            arguments: serde_json::json!({ "path": path, "content": "…" }),
        })],
        "faux-1",
        Api::Faux,
        Usage::default(),
        feather_core::message::StopReason::ToolUse,
    )
}

#[tokio::test]
async fn threshold_compaction_rewrites_context() {
    let root = temp_root("auto");
    let mut session = Session::new(&root.join("session"), &root).unwrap();

    // --- seed 3 heavy turns (assistant write-call + big tool result each)
    for i in 0..3 {
        session
            .push(Message::user_text(format!(
                "turn {i} question about module_{i}"
            )))
            .unwrap();
        session
            .push(asst_write(&format!("src/module_{i}.rs")))
            .unwrap();
        let mut tr = Message::tool_result(
            format!("call-src/module_{i}.rs"),
            "write",
            "w".repeat(4_000), // ≈1k tokens each
            false,
        );
        tr.usage = Some(Usage {
            input: 0,
            output: 0,
            cache_read: 0,
            cache_write: 0,
        });
        session.push(tr).unwrap();
        session.commit_turn().unwrap();
    }
    let seeded_len = session.messages().len();
    assert_eq!(seeded_len, 9);
    let seeded_raw = session.raw_messages().unwrap();

    // --- faux: first turn answers the summary request, second the real one
    let script = FauxScript::new()
        .with_text("mock summary of the earlier work")
        .with_text("hello after compaction");
    let provider = FauxProvider::new(script);

    let config = AgentConfig {
        thinking_level: None,
        model: "faux-1".into(),
        api: Api::AnthropicMessages,
        system_prompt: "e2e".into(),
        max_tokens: 1024,
        temperature: None,
        // threshold = 1_000 - 100 = 900 tokens: the seeded ~3k tokens trigger
        context_window: 1_000,
        compaction: CompactionSettings {
            enabled: true,
            reserve_tokens: 100,
            // keep exactly the last heavy turn (~1.1k tokens) in the kept span
            keep_recent_tokens: 1_500,
            summary_tool_result_budget: 500,
            kept_tool_result_budget: 800,
            shrink_kept: true,
            keep_last_turns_unshrunk: 1,
        },
        compaction_model: None,
        tools: Vec::new().into(),
    };
    let mut agent = Agent::new(
        config,
        session,
        Arc::new(feather_core::tool::ToolRegistry::new()),
        ProviderRef::Faux(provider),
        None,
    );
    let mut events = agent.subscribe();

    let res = agent.prompt("final question").await.expect("run completes");
    assert_eq!(res.final_text, "hello after compaction");

    // --- linear context rewritten: summary first, compacted, recent kept
    let msgs = agent.session().messages().to_vec();
    let first = msgs[0].text();
    assert!(
        first.starts_with("## Conversation summary (feather compaction)"),
        "summary leads the context:\n{}",
        &first[..80.min(first.len())]
    );
    assert!(first.contains("mock summary of the earlier work"));
    // file_ops picked the write targets out of the summarized span
    assert!(first.contains("<modified-files>"));
    assert!(first.contains("src/module_0.rs"));

    // context actually shrank
    assert!(
        msgs.len() < 9,
        "linear context compacted: {} rows",
        msgs.len()
    );

    // the new prompt survived the rewrite (the assistant reply trails it)
    assert!(
        msgs.iter()
            .any(|m| m.role == feather_core::message::Role::User && m.text() == "final question"),
        "live prompt kept after compaction"
    );
    assert_eq!(msgs.last().unwrap().text(), "hello after compaction");

    // the kept span's old tool results are shrunk to their budget
    let kept_results: Vec<&Message> = msgs
        .iter()
        .filter(|m| m.role == feather_core::message::Role::ToolResult)
        .collect();
    assert!(
        !kept_results.is_empty(),
        "recent turn's tool result kept (shrunk)"
    );
    for m in kept_results {
        assert!(
            m.text().len() < 1_200,
            "kept result shrunk to budget, got {}",
            m.text().len()
        );
    }

    // --- raw redb audit untouched: every seeded row still present
    let now_raw = agent.session().raw_messages().unwrap();
    for m in &seeded_raw {
        assert!(
            now_raw.iter().any(|x| x == m),
            "raw audit lost a seeded message"
        );
    }

    // --- CompactionDone surfaced
    let mut compacted = false;
    while let Ok(ev) = events.try_recv() {
        if let AgentEvent::CompactionDone {
            tokens_before,
            tokens_after,
            ..
        } = ev
        {
            assert!(tokens_before > tokens_after, "context shrank");
            compacted = true;
        }
    }
    assert!(compacted, "CompactionDone event observed");

    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn manual_compact_is_idempotent_when_nothing_to_summarize() {
    let root = temp_root("manual");
    let mut session = Session::new(&root.join("session"), &root).unwrap();
    session.push(Message::user_text("tiny")).unwrap();
    session.commit_turn().unwrap();

    let script = FauxScript::new().with_text("unused");
    let provider = FauxProvider::new(script);
    let config = AgentConfig {
        thinking_level: None,
        model: "faux-1".into(),
        api: Api::AnthropicMessages,
        system_prompt: "e2e".into(),
        max_tokens: 128,
        temperature: None,
        context_window: 200_000,
        compaction: CompactionSettings {
            enabled: true,
            reserve_tokens: 16_384,
            keep_recent_tokens: 20_000,
            summary_tool_result_budget: 2_000,
            kept_tool_result_budget: 4_000,
            shrink_kept: true,
            keep_last_turns_unshrunk: 2,
        },
        compaction_model: None,
        tools: Vec::new().into(),
    };
    let mut agent = Agent::new(
        config,
        session,
        Arc::new(feather_core::tool::ToolRegistry::new()),
        ProviderRef::Faux(provider),
        None,
    );
    // cut == 0 (nothing beyond the kept-recent span) → no-op, no provider call
    let saved = agent.compact("manual").await.expect("compact is a no-op");
    assert_eq!(saved, 0);
    assert_eq!(agent.session().messages().len(), 1);

    let _ = std::fs::remove_dir_all(&root);
}
