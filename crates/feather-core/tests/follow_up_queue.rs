use feather_core::{
    agent::{Agent, AgentConfig, AgentEvent, ProviderRef},
    compaction::CompactionSettings,
    message::{Api, Message, Role},
    provider::faux::{FauxProvider, FauxScript},
    session::Session,
    tool::{AgentTool, BoxFut, ToolOutcome, ToolRegistry},
};
use std::sync::Arc;

fn config() -> AgentConfig {
    AgentConfig {
        model: "test".into(),
        api: Api::Faux,
        system_prompt: "test".into(),
        max_tokens: 1024,
        temperature: None,
        context_window: 200_000,
        thinking_level: None,
        compaction: CompactionSettings::default(),
        compaction_model: None,
        tools: vec![].into(),
    }
}

fn session(name: &str) -> (Session, std::path::PathBuf) {
    let root = std::env::temp_dir().join(feather_core::provider::gen_id(name));
    std::fs::create_dir_all(&root).unwrap();
    (Session::new(&root.join(".feather"), &root).unwrap(), root)
}

struct Gate {
    tx: tokio::sync::watch::Sender<bool>,
}
impl AgentTool for Gate {
    fn name(&self) -> &'static str {
        "gate"
    }
    fn description(&self) -> &'static str {
        "waits"
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({"type":"object"})
    }
    fn execute<'a>(
        &'a self,
        _args: serde_json::Value,
        mut abort: tokio::sync::watch::Receiver<bool>,
    ) -> BoxFut<'a> {
        let mut rx = self.tx.subscribe();
        Box::pin(async move {
            loop {
                if *abort.borrow() {
                    return ToolOutcome::err("aborted");
                }
                if *rx.borrow() {
                    return ToolOutcome::ok("opened");
                }
                tokio::select! {
                    _ = rx.changed() => {},
                    _ = abort.changed() => {},
                }
            }
        })
    }
}

#[tokio::test]
async fn follow_up_queued_during_tool_runs_after_current_final() {
    let (session, root) = session("follow-tool");
    let (open, _) = tokio::sync::watch::channel(false);
    let mut tools = ToolRegistry::new();
    tools.add(Arc::new(Gate { tx: open.clone() }));
    let script = FauxScript::new()
        .with_tool("gate", serde_json::json!({}))
        .with_text("current final")
        .with_text("follow final");
    let mut agent = Agent::new(
        config(),
        session,
        Arc::new(tools),
        ProviderRef::Faux(FauxProvider::new(script)),
        None,
    );
    let queue = agent.input_queue();
    let mut events = agent.subscribe();
    let task = tokio::spawn(async move {
        let r = agent.prompt("initial").await;
        (agent, r)
    });

    while !matches!(events.recv().await.unwrap(), AgentEvent::ToolStart { .. }) {}
    queue.push(Message::user_text("queued follow"), false);
    open.send(true).unwrap();
    let (agent, result) = task.await.unwrap();
    assert_eq!(result.unwrap().final_text, "follow final");

    let texts: Vec<_> = agent
        .session()
        .messages()
        .iter()
        .map(|m| (m.role, m.text()))
        .collect();
    assert_eq!(
        texts.iter().filter(|(_, t)| t == "queued follow").count(),
        1
    );
    let current = texts
        .iter()
        .position(|(_, t)| t == "current final")
        .unwrap();
    let queued = texts
        .iter()
        .position(|(_, t)| t == "queued follow")
        .unwrap();
    let follow = texts.iter().position(|(_, t)| t == "follow final").unwrap();
    assert!(current < queued && queued < follow, "{texts:?}");
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn final_assistant_event_precedes_queued_user_and_next_turn() {
    let (session, root) = session("follow-order");
    let script = FauxScript::new()
        .with_text("current final")
        .with_text("follow final");
    let mut agent = Agent::new(
        config(),
        session,
        Arc::new(ToolRegistry::new()),
        ProviderRef::Faux(FauxProvider::new(script)),
        None,
    );
    agent.queue_follow_up("queued follow");
    let mut events = agent.subscribe();
    agent.prompt("initial").await.unwrap();
    let mut seq = Vec::new();
    while let Ok(ev) = events.try_recv() {
        match ev {
            AgentEvent::AssistantMessage(m) => seq.push(format!("{:?}:{}", m.role, m.text())),
            AgentEvent::RunEnd(r) => seq.push(format!("RunEnd:{}", r.final_text)),
            AgentEvent::TurnStart(_) => seq.push("TurnStart".into()),
            _ => {}
        }
    }
    let cur = seq
        .iter()
        .position(|s| s == "Assistant:current final")
        .unwrap();
    let run = seq
        .iter()
        .position(|s| s == "RunEnd:current final")
        .unwrap();
    let queued = seq.iter().position(|s| s == "User:queued follow").unwrap();
    let next_turn = seq
        .iter()
        .enumerate()
        .skip(queued)
        .find(|(_, s)| *s == "TurnStart")
        .unwrap()
        .0;
    assert!(cur < run && run < queued && queued < next_turn, "{seq:?}");
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn multiple_followups_run_one_per_request_in_order() {
    let (session, root) = session("follow-many");
    let script = FauxScript::new()
        .with_text("a0")
        .with_text("a1")
        .with_text("a2");
    let mut agent = Agent::new(
        config(),
        session,
        Arc::new(ToolRegistry::new()),
        ProviderRef::Faux(FauxProvider::new(script)),
        None,
    );
    agent.queue_follow_up("f1");
    agent.queue_follow_up("f2");
    agent.prompt("initial").await.unwrap();
    let users: Vec<_> = agent
        .session()
        .messages()
        .iter()
        .filter(|m| m.role == Role::User)
        .map(Message::text)
        .collect();
    assert_eq!(users, ["initial", "f1", "f2"]);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn queued_followup_survives_abort_before_delivery() {
    let (session, root) = session("follow-abort");
    let script = FauxScript::new().with_text("never mind");
    let mut agent = Agent::new(
        config(),
        session,
        Arc::new(ToolRegistry::new()),
        ProviderRef::Faux(FauxProvider::new(script)),
        None,
    );
    agent.queue_follow_up("survivor");
    agent.abort();
    let result = agent
        .prompt_message_prepared(Message::user_text("initial"))
        .await
        .unwrap();
    assert!(matches!(
        result.stop_reason,
        feather_core::message::StopReason::Aborted
    ));
    assert_eq!(agent.input_queue().snapshot().1, vec!["survivor"]);
    std::fs::remove_dir_all(root).unwrap();
}
