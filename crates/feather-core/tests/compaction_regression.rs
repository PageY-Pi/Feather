//! Compaction is maintenance, not an assistant turn. It must never leak summary
//! deltas, replace a visible final answer, or claim a refresh for a no-op.
use feather_core::{
    agent::{AgentConfig, AgentEvent, ProviderRef},
    compaction::{self, CompactionSettings},
    message::{Api, ContentBlock, Message, Role, StopReason, Usage},
    provider::faux::{FauxProvider, FauxScript},
    tool::ToolRegistry,
    Agent, Session,
};
use std::{path::Path, sync::Arc};

fn answer(text: &str) -> Message {
    Message::assistant(
        vec![ContentBlock::Text { text: text.into() }],
        "faux",
        Api::Faux,
        Usage::default(),
        StopReason::Stop,
    )
}
fn config(window: u64, keep: u64) -> AgentConfig {
    AgentConfig {
        model: "faux".into(),
        api: Api::Faux,
        system_prompt: "test".into(),
        max_tokens: 1024,
        temperature: None,
        context_window: window,
        thinking_level: None,
        compaction: CompactionSettings {
            reserve_tokens: 100,
            keep_recent_tokens: keep,
            ..Default::default()
        },
        compaction_model: None,
        tools: vec![].into(),
    }
}
fn seeded() -> Session {
    let mut session = Session::memory(Path::new("."), "compaction-regression");
    session.push(Message::user_text("old question")).unwrap();
    session.push(answer(&"old answer ".repeat(2000))).unwrap();
    session.push(Message::user_text("recent question")).unwrap();
    session.push(answer("PRIOR FINAL PURE TEXT")).unwrap();
    session
}
fn agent(session: Session, window: u64, keep: u64, script: FauxScript) -> Agent {
    Agent::new(
        config(window, keep),
        session,
        Arc::new(ToolRegistry::new()),
        ProviderRef::Faux(FauxProvider::new(script)),
        None,
    )
}
fn drain(events: &mut tokio::sync::broadcast::Receiver<AgentEvent>) -> Vec<AgentEvent> {
    let mut out = vec![];
    while let Ok(event) = events.try_recv() {
        out.push(event);
    }
    out
}

#[tokio::test]
async fn auto_summary_is_private_and_preserves_latest_completed_answer() {
    let mut agent = agent(
        seeded(),
        1000,
        1,
        FauxScript::new()
            .with_text("INTERNAL SUMMARY")
            .with_text("NEW FINAL PURE TEXT"),
    );
    let mut events = agent.subscribe();
    let result = agent.prompt("next question").await.unwrap();
    assert_eq!(result.final_text, "NEW FINAL PURE TEXT");
    assert!(
        agent
            .session()
            .messages()
            .iter()
            .any(|m| m.role == Role::Assistant && m.text() == "PRIOR FINAL PURE TEXT"),
        "latest completed answer must survive the context rewrite"
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
    assert_eq!(
        deltas,
        vec!["NEW FINAL PURE TEXT"],
        "summaries must not occupy or replace live response rows"
    );
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, AgentEvent::TurnStart(_)))
            .count(),
        1
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e,AgentEvent::CompactionDone{chars_saved,..} if *chars_saved>0)),
        "pure-text summarization must report actual savings"
    );
}

#[tokio::test]
async fn threshold_noop_reports_unavoidable_over_limit_without_repeating_compaction() {
    let session = Session::memory(Path::new("."), "compaction-noop");
    let mut agent = agent(
        session,
        1000,
        100_000,
        FauxScript::new().with_text("ONLY FINAL"),
    );
    let mut events = agent.subscribe();
    let error = agent.prompt(&"question ".repeat(1000)).await.unwrap_err();
    assert!(error
        .to_string()
        .contains("context remains over model limit"));
    assert!(
        !drain(&mut events)
            .iter()
            .any(|e| matches!(e, AgentEvent::CompactionDone { .. })),
        "an ineffective compaction must not report repeated refreshes"
    );
}

#[tokio::test]
async fn manual_text_compaction_counts_unicode_characters_and_never_streams_a_reply() {
    let mut session = Session::memory(Path::new("."), "unicode-compaction");
    session
        .push(Message::user_text("想法".repeat(3000)))
        .unwrap();
    session.push(answer("早先的结果")).unwrap();
    session.push(Message::user_text("recent question")).unwrap();
    session.push(answer("LATEST FINAL")).unwrap();
    let chars = |messages: &[Message]| {
        messages
            .iter()
            .flat_map(|m| m.content.iter())
            .map(|b| match b {
                ContentBlock::Text { text } | ContentBlock::Thinking { text, .. } => {
                    text.chars().count()
                }
                _ => 0,
            })
            .sum::<usize>()
    };
    let before = chars(session.messages());
    let mut agent = agent(
        session,
        100_000,
        1,
        FauxScript::new().with_text("short summary"),
    );
    let mut events = agent.subscribe();
    let saved = agent.compact("manual").await.unwrap();
    assert_eq!(
        saved,
        before - chars(agent.session().messages()),
        "characters saved must include summarization, not just kept tool-result bytes"
    );
    assert!(saved > 0);
    assert!(
        drain(&mut events).is_empty(),
        "manual summarization must never be an assistant response"
    );
    assert_eq!(
        agent.session().messages().last().unwrap().text(),
        "LATEST FINAL"
    );
}

#[tokio::test]
async fn empty_or_oversized_summary_cannot_destroy_context() {
    for summary in [String::new(), "oversized summary ".repeat(10_000)] {
        let session = seeded();
        let original = session.messages().to_vec();
        let mut agent = agent(session, 100_000, 1, FauxScript::new().with_text(summary));
        let mut events = agent.subscribe();
        let result = agent.compact("manual").await;
        assert!(result.is_err() || result.as_ref().is_ok_and(|saved| *saved == 0));
        assert_eq!(
            agent.session().messages(),
            original.as_slice(),
            "invalid/no-saving summary must not rewrite the context"
        );
        assert!(
            drain(&mut events).is_empty(),
            "failed maintenance must not masquerade as live output"
        );
    }
}

#[test]
fn cut_keeps_latest_completed_span_even_when_active_prompt_exceeds_budget() {
    let mut session = seeded();
    session
        .push(Message::user_text("current prompt ".repeat(1000)))
        .unwrap();
    assert_eq!(
        compaction::plan_cut(session.messages(), 1),
        2,
        "keep both the latest completed response and current user span"
    );
}

#[tokio::test]
async fn compaction_reopen_keeps_previous_final_and_current_prompt() {
    let root = std::env::temp_dir().join(format!(
        "feather-compaction-reopen-{}-{}",
        std::process::id(),
        feather_core::provider::gen_id("test")
    ));
    std::fs::create_dir_all(&root).unwrap();
    let home = root.join("home");
    let mut session = Session::new(&home, &root).unwrap();
    for message in seeded().messages() {
        session.push(message.clone()).unwrap();
    }
    session.push(Message::user_text("CURRENT PROMPT")).unwrap();
    session.commit_turn().unwrap();
    let dir = session.dir().to_path_buf();
    let mut agent = agent(
        session,
        100_000,
        1,
        FauxScript::new().with_text("short summary"),
    );
    assert!(agent.compact("manual").await.unwrap() > 0);
    drop(agent);
    let reopened = Session::open(&dir, &home).unwrap();
    assert!(reopened
        .messages()
        .iter()
        .any(|m| m.text() == "PRIOR FINAL PURE TEXT"));
    assert_eq!(reopened.messages().last().unwrap().text(), "CURRENT PROMPT");
    assert!(reopened
        .raw_messages()
        .unwrap()
        .iter()
        .any(|m| m.text() == "PRIOR FINAL PURE TEXT"));
    drop(reopened);
    std::fs::remove_dir_all(root).unwrap();
}
