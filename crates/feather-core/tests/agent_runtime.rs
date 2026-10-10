//! Focused runtime-agent tests: `reconfigure` swaps model/provider/tools while
//! keeping the session and planner, an abort cancels a compaction before it
//! rewrites context, and a queued steer message is re-emitted as a finalized
//! message carrying its image block.

use feather_core::agent::{Agent, AgentConfig, AgentError, AgentEvent, ProviderRef};
use feather_core::compaction::CompactionSettings;
use feather_core::message::{Api, ContentBlock, Message, Role, StopReason, Usage};
use feather_core::provider::faux::{FauxProvider, FauxScript};
use feather_core::session::Session;
use feather_core::tool::ToolRegistry;
use std::sync::Arc;

fn temp(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!(
        "feather-agent-{tag}-{}-{}",
        std::process::id(),
        feather_core::message::now_millis()
    ));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn config(model: &str) -> AgentConfig {
    AgentConfig {
        model: model.into(),
        api: Api::Faux,
        system_prompt: "test".into(),
        max_tokens: 1024,
        temperature: None,
        context_window: 200_000,
        thinking_level: None,
        compaction: CompactionSettings::default(),
        compaction_model: None,
        tools: Vec::new().into(),
    }
}

#[tokio::test]
async fn reconfigure_swaps_model_and_provider_without_touching_session() {
    let root = temp("reconfigure");
    let mut session = Session::new(&root.join(".feather"), &root).unwrap();
    session.push(Message::user_text("original")).unwrap();
    session.commit_turn().unwrap();
    let session_id = session.id().to_string();
    let before = session.messages().to_vec();

    let tools = Arc::new(ToolRegistry::new());
    let mut agent = Agent::new(
        config("faux-a"),
        session,
        tools.clone(),
        ProviderRef::Faux(FauxProvider::new(FauxScript::new().with_text("a"))),
        None,
    );
    assert_eq!(agent.model_id(), "faux-a");

    agent.reconfigure(
        config("faux-b"),
        ProviderRef::Faux(FauxProvider::new(FauxScript::new().with_text("b"))),
        tools.clone(),
    );

    assert_eq!(agent.model_id(), "faux-b");
    assert_eq!(
        agent.session().id(),
        session_id,
        "session identity is preserved"
    );
    assert_eq!(
        agent.session().messages(),
        before.as_slice(),
        "history is preserved"
    );

    let result = agent.prompt("hello").await.unwrap();
    assert_eq!(
        result.final_text, "b",
        "the new provider serves the next turn"
    );

    drop(agent);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn abort_cancels_compaction_before_it_rewrites_context() {
    let root = temp("compact-abort");
    let mut session = Session::new(&root.join(".feather"), &root).unwrap();
    for i in 0..3 {
        session
            .push(Message::user_text(format!(
                "question {i} {}",
                "x".repeat(4_000)
            )))
            .unwrap();
        session
            .push(Message::assistant(
                vec![ContentBlock::Text {
                    text: "answer".into(),
                }],
                "faux-a",
                Api::Faux,
                Usage::default(),
                StopReason::Stop,
            ))
            .unwrap();
        session.commit_turn().unwrap();
    }
    let before = session.messages().to_vec();

    let settings = CompactionSettings {
        enabled: true,
        reserve_tokens: 16_384,
        // force a non-empty summarize span if the abort did not short-circuit
        keep_recent_tokens: 1,
        summary_tool_result_budget: 2_000,
        kept_tool_result_budget: 4_000,
        shrink_kept: true,
        keep_last_turns_unshrunk: 0,
    };
    let mut agent = Agent::new(
        AgentConfig {
            compaction: settings,
            ..config("faux-a")
        },
        session,
        Arc::new(ToolRegistry::new()),
        ProviderRef::Faux(FauxProvider::new(FauxScript::new().with_text("summary"))),
        None,
    );

    agent.abort();
    let err = agent.compact("manual").await.unwrap_err();
    assert!(
        matches!(err, AgentError::Aborted),
        "expected abort, got {err:?}"
    );
    assert_eq!(
        agent.session().messages(),
        before.as_slice(),
        "an aborted compaction must not rewrite the context"
    );

    drop(agent);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn queued_user_message_is_emitted_as_finalized_message_with_image() {
    let root = temp("queue-event");
    let session = Session::new(&root.join(".feather"), &root).unwrap();
    let script = FauxScript::new()
        .with_tool("missing", serde_json::json!({}))
        .with_text("steered");
    let mut agent = Agent::new(
        config("faux-a"),
        session,
        Arc::new(ToolRegistry::new()),
        ProviderRef::Faux(FauxProvider::new(script)),
        None,
    );
    let mut events = agent.subscribe();

    let mut queued = Message::user_text("steer");
    queued.content.push(ContentBlock::Image {
        media_type: "image/png".into(),
        data: "AAAA".into(),
    });
    agent.input_queue().push(queued, true);

    let _ = agent.prompt("initial").await.unwrap();

    let mut seen = false;
    while let Ok(ev) = events.try_recv() {
        if let AgentEvent::AssistantMessage(m) = ev {
            if m.role == Role::User {
                seen = true;
                assert!(
                    matches!(m.content.last(), Some(ContentBlock::Image { .. })),
                    "queued message keeps its image block: {:?}",
                    m.content
                );
            }
        }
    }
    assert!(seen, "queued steering message was re-emitted");

    drop(agent);
    std::fs::remove_dir_all(root).unwrap();
}
