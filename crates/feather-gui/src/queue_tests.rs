use super::*;
use feather_core::{
    agent::{AgentConfig, ProviderRef},
    compaction::CompactionSettings,
    message::{Api, ContentBlock, Message, Usage},
    provider::faux::{FauxProvider, FauxScript},
    tool::{AgentTool, BoxFut, ToolOutcome, ToolRegistry},
    Session,
};
struct Gate(tokio::sync::watch::Sender<bool>);
impl AgentTool for Gate {
    fn name(&self) -> &'static str {
        "gate"
    }
    fn description(&self) -> &'static str {
        "gate"
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({"type":"object"})
    }
    fn execute<'a>(
        &'a self,
        _: serde_json::Value,
        _: tokio::sync::watch::Receiver<bool>,
    ) -> BoxFut<'a> {
        let mut rx = self.0.subscribe();
        Box::pin(async move {
            while !*rx.borrow() {
                rx.changed().await.unwrap();
            }
            ToolOutcome::ok("released")
        })
    }
}
fn make_agent(tools: ToolRegistry, script: FauxScript) -> Agent {
    let defs = tools.defs();
    Agent::new(
        AgentConfig {
            model: "test".into(),
            api: Api::Faux,
            system_prompt: "test".into(),
            max_tokens: 1000,
            temperature: None,
            context_window: 200000,
            thinking_level: None,
            compaction: Default::default(),
            compaction_model: None,
            tools: defs.into(),
        },
        Session::memory(std::path::Path::new("fixture"), "queue-test"),
        Arc::new(tools),
        ProviderRef::Faux(FauxProvider::new(script)),
        None,
    )
}
fn wait_events(app: &mut FeatherApp, predicate: impl Fn(&FeatherApp) -> bool) {
    let started = std::time::Instant::now();
    loop {
        app.drain_events();
        if predicate(app) {
            break;
        }
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
}
#[test]
fn queued_prompt_waits_for_final_and_only_task_completion_marks_idle() {
    let mut app = crate::tests::fixture();
    let (open, _) = tokio::sync::watch::channel(false);
    let mut tools = ToolRegistry::new();
    tools.add(Arc::new(Gate(open.clone())));
    let agent = make_agent(
        tools,
        FauxScript::new()
            .with_tool("gate", serde_json::json!({}))
            .with_text("CURRENT FINAL")
            .with_text("FOLLOW FINAL"),
    );
    app.input_queue = agent.input_queue();
    app.abort_tx = agent.abort_sender();
    app.agent = Some(Arc::new(tokio::sync::Mutex::new(agent)));
    app.input = "initial".into();
    app.submit();
    wait_events(&mut app, |app| {
        app.rows.iter().any(|r| r.kind == RowKind::Tool)
    });
    app.input = "next task".into();
    app.submit();
    assert!(app.running);
    assert_eq!(app.queued_hint, 1);
    assert!(!app.rows.iter().any(|r| r.text == "next task"));
    open.send(true).unwrap();
    wait_events(&mut app, |app| !app.running);
    let messages: Vec<_> = app
        .rows
        .iter()
        .filter(|r| matches!(r.kind, RowKind::User | RowKind::Assistant))
        .map(|r| r.text.as_str())
        .collect();
    assert_eq!(
        messages,
        vec!["initial", "CURRENT FINAL", "next task", "FOLLOW FINAL"]
    );
    assert_eq!(app.queued_hint, 0);
    let result = RunResult {
        stop_reason: StopReason::Stop,
        turns: 1,
        usage: Default::default(),
        final_text: "done".into(),
    };
    app.running = true;
    app.on_agent_event(AgentEvent::RunEnd(result));
    assert!(
        app.running,
        "RunEnd isn't completion of the queued prompt future"
    );
}
#[test]
fn late_follow_up_at_completion_is_not_stranded_or_added_without_request() {
    let mut app = crate::tests::fixture();
    let agent = make_agent(
        ToolRegistry::new(),
        FauxScript::new().with_text("LATE FINAL"),
    );
    app.input_queue = agent.input_queue();
    app.agent = Some(Arc::new(tokio::sync::Mutex::new(agent)));
    app.running = true;
    app.input = "late task".into();
    app.submit();
    let result = RunResult {
        stop_reason: StopReason::Stop,
        turns: 1,
        usage: Default::default(),
        final_text: "prior final".into(),
    };
    app.rows
        .insert(0, ChatRow::new(RowKind::Assistant, "prior final"));
    app.ev_tx.send(UiEvent::RunDone(Ok(result))).unwrap();
    app.drain_events();
    wait_events(&mut app, |a| !a.running);
    assert!(app.rows.iter().any(|r| r.text == "LATE FINAL"));
    assert!(app.input_queue.snapshot().1.is_empty());
}

#[test]
fn abort_between_acceptance_and_first_poll_is_not_reset() {
    let mut app = crate::tests::fixture();
    let agent = make_agent(
        ToolRegistry::new(),
        FauxScript::new().with_text("MUST NOT RUN"),
    );
    app.input_queue = agent.input_queue();
    app.abort_tx = agent.abort_sender();
    let agent = Arc::new(tokio::sync::Mutex::new(agent));
    app.agent = Some(agent.clone());
    let lock = app.rt.block_on(agent.lock());
    app.input = "cancel immediately".into();
    app.submit();
    app.abort();
    drop(lock);
    wait_events(&mut app, |app| !app.running);
    assert_eq!(app.last_stop, Some(StopReason::Aborted));
    assert!(!app.rows.iter().any(|r| r.text == "MUST NOT RUN"));
}

#[test]
fn auto_compaction_preserves_plain_text_rows_without_summary_leaks() {
    let mut session = Session::memory(std::path::Path::new("fixture"), "debug-compact");
    let big = "x".repeat(8000);
    session.push(Message::user_text("first question")).unwrap();
    session
        .push(Message::assistant(
            vec![ContentBlock::Text { text: big.clone() }],
            "test",
            Api::Faux,
            Usage::default(),
            StopReason::Stop,
        ))
        .unwrap();
    session.push(Message::user_text("second question")).unwrap();
    session
        .push(Message::assistant(
            vec![ContentBlock::Text { text: big.clone() }],
            "test",
            Api::Faux,
            Usage::default(),
            StopReason::Stop,
        ))
        .unwrap();
    session
        .push(Message::assistant(
            vec![ContentBlock::Text {
                text: "PRIOR FINAL".into(),
            }],
            "test",
            Api::Faux,
            Usage::default(),
            StopReason::Stop,
        ))
        .unwrap();
    let script = FauxScript::new()
        .with_text("SUMMARYMARKER should never show")
        .with_text("NEW FINAL");
    let agent = make_agent_seeded(session, 1000, 1, script);
    let mut app = crate::tests::fixture();
    app.input_queue = agent.input_queue();
    app.abort_tx = agent.abort_sender();
    app.rows
        .push(ChatRow::new(RowKind::Assistant, "PRIOR FINAL"));
    app.agent = Some(Arc::new(tokio::sync::Mutex::new(agent)));
    app.input = "new question".into();
    app.submit();
    wait_events(&mut app, |app| !app.running);
    assert_eq!(
        app.rows
            .iter()
            .filter(|r| r.kind == RowKind::Assistant && r.text == "PRIOR FINAL")
            .count(),
        1
    );
    assert_eq!(
        app.rows
            .iter()
            .filter(|r| r.kind == RowKind::Assistant && r.text == "NEW FINAL")
            .count(),
        1
    );
    assert!(!app
        .rows
        .iter()
        .any(|r| r.text.contains("SUMMARYMARKER") || r.streaming));
    assert!(app.rows.iter().any(|r| r.kind == RowKind::Notice
        && r.text.starts_with("Context compacted")
        && !r.text.contains("· 0 characters")));
}

fn make_agent_seeded(
    session: Session,
    context_window: u64,
    keep_recent_tokens: u64,
    script: FauxScript,
) -> Agent {
    let tools = ToolRegistry::new();
    let defs = tools.defs();
    Agent::new(
        AgentConfig {
            model: "test".into(),
            api: Api::Faux,
            system_prompt: "test".into(),
            max_tokens: 1000,
            temperature: None,
            context_window,
            thinking_level: None,
            compaction: CompactionSettings {
                enabled: true,
                reserve_tokens: 100,
                keep_recent_tokens,
                summary_tool_result_budget: 2000,
                kept_tool_result_budget: 4000,
                shrink_kept: true,
                keep_last_turns_unshrunk: 1,
            },
            compaction_model: None,
            tools: defs.into(),
        },
        session,
        Arc::new(tools),
        ProviderRef::Faux(FauxProvider::new(script)),
        None,
    )
}

#[test]
fn noop_compaction_does_not_create_refresh_or_replace_last_answer() {
    let agent = make_agent_seeded(
        Session::memory(std::path::Path::new("fixture"), "noop"),
        10_000,
        1,
        FauxScript::new().with_text("ONLY FINAL"),
    );
    let mut app = crate::tests::fixture();
    app.input_queue = agent.input_queue();
    app.abort_tx = agent.abort_sender();
    app.agent = Some(Arc::new(tokio::sync::Mutex::new(agent)));
    app.input = "question ".repeat(1000);
    app.submit();
    wait_events(&mut app, |app| !app.running);
    assert_eq!(
        app.rows
            .iter()
            .filter(|r| r.kind == RowKind::Assistant && r.text == "ONLY FINAL")
            .count(),
        1
    );
    assert!(!app
        .rows
        .iter()
        .any(|r| r.kind == RowKind::Notice || r.streaming));
}

#[test]
fn compaction_notice_is_not_an_answer_and_new_turn_never_reuses_prior_row() {
    let mut app = crate::tests::fixture();
    let mut prior = ChatRow::new(RowKind::Assistant, "PRIOR FINAL");
    prior.streaming = true;
    app.rows.push(prior);
    app.on_agent_event(AgentEvent::CompactionDone {
        tokens_before: 100,
        tokens_after: 100,
        chars_saved: 0,
    });
    assert_eq!(app.rows.len(), 1);
    assert_eq!(app.rows[0].text, "PRIOR FINAL");
    app.on_agent_event(AgentEvent::TurnStart(2));
    app.on_agent_event(AgentEvent::TextDelta("NEW FINAL".into()));
    app.on_agent_event(AgentEvent::AssistantMessage(Message::assistant(
        vec![ContentBlock::Text {
            text: "NEW FINAL".into(),
        }],
        "test",
        Api::Faux,
        Usage::default(),
        StopReason::Stop,
    )));
    assert_eq!(app.rows.len(), 2);
    assert_eq!(app.rows[0].text, "PRIOR FINAL");
    assert_eq!(app.rows[1].text, "NEW FINAL");
    assert!(app.rows.iter().all(|r| !r.streaming));
}
