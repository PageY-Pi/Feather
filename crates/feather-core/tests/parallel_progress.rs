use feather_core::{
    agent::{Agent, AgentConfig, AgentEvent, ProviderRef},
    provider::openai::OpenAiProvider,
    tool::{AgentTool, BoxFut, ToolOutcome, ToolRegistry},
    Session,
};
use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::Arc,
};
struct Child(Arc<tokio::sync::Barrier>);
impl AgentTool for Child {
    fn name(&self) -> &'static str {
        "subagent"
    }
    fn description(&self) -> &'static str {
        "synthetic parallel child"
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({"type":"object","properties":{}})
    }
    fn parallel_safe(&self) -> bool {
        true
    }
    fn execute<'a>(
        &'a self,
        _args: serde_json::Value,
        _abort: tokio::sync::watch::Receiver<bool>,
    ) -> BoxFut<'a> {
        Box::pin(async move {
            self.0.wait().await;
            ToolOutcome::ok("bounded child result")
        })
    }
    fn execute_with_progress<'a>(
        &'a self,
        args: serde_json::Value,
        abort: tokio::sync::watch::Receiver<bool>,
        progress: tokio::sync::mpsc::UnboundedSender<String>,
    ) -> BoxFut<'a> {
        Box::pin(async move {
            progress.send("child is running".into()).unwrap();
            self.execute(args, abort).await
        })
    }
}
#[tokio::test]
async fn parallel_children_emit_progress_and_parent_results_are_redb_ordered() {
    let root = std::env::temp_dir().join(feather_core::provider::gen_id("parallel-parent"));
    std::fs::create_dir_all(&root).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        for turn in 0..2 {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(std::time::Duration::from_secs(4)))
                .unwrap();
            let mut raw = Vec::new();
            loop {
                let mut buf = [0; 4096];
                let n = socket.read(&mut buf).unwrap();
                assert!(n > 0);
                raw.extend_from_slice(&buf[..n]);
                if let Some(end) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
                    let head = String::from_utf8_lossy(&raw[..end]);
                    let len: usize = head
                        .lines()
                        .find_map(|l| {
                            l.split_once(':')
                                .filter(|(k, _)| k.eq_ignore_ascii_case("content-length"))
                                .map(|(_, v)| v.trim().parse().unwrap())
                        })
                        .unwrap();
                    if raw.len() >= end + 4 + len {
                        break;
                    }
                }
            }
            let data = if turn == 0 {
                "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"a\",\"type\":\"function\",\"function\":{\"name\":\"subagent\",\"arguments\":\"{}\"}},{\"index\":1,\"id\":\"b\",\"type\":\"function\",\"function\":{\"name\":\"subagent\",\"arguments\":\"{}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\ndata: [DONE]\n\n"
            } else {
                "data: {\"choices\":[{\"delta\":{\"content\":\"done\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n"
            };
            write!(socket,"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",data.len(),data).unwrap();
        }
    });
    let mut registry = ToolRegistry::new();
    registry.add(Arc::new(Child(Arc::new(tokio::sync::Barrier::new(2)))));
    let defs = registry.defs();
    let session = Session::new(&root.join("home"), &root).unwrap();
    let dir = session.dir().to_path_buf();
    let mut agent = Agent::new(
        AgentConfig {
            model: "synthetic".into(),
            api: feather_core::message::Api::OpenaiCompletions,
            system_prompt: "test".into(),
            max_tokens: 32,
            temperature: None,
            context_window: 10000,
            thinking_level: None,
            compaction: Default::default(),
            compaction_model: None,
            tools: defs.into(),
        },
        session,
        Arc::new(registry),
        ProviderRef::OpenAi(Arc::new(OpenAiProvider::new("synthetic", Some(base)))),
        None,
    );
    let mut events = agent.subscribe();
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        agent.prompt("parallel synthetic task"),
    )
    .await
    .unwrap()
    .unwrap();
    let mut progress = 0;
    while let Ok(event) = events.try_recv() {
        if matches!(event, AgentEvent::ToolProgress { .. }) {
            progress += 1;
        }
    }
    assert!(progress >= 2);
    let raw = agent.session().raw_messages().unwrap();
    assert_eq!(raw[2].tool_call_id.as_deref(), Some("a"));
    assert_eq!(raw[3].tool_call_id.as_deref(), Some("b"));
    assert_eq!(raw[2].text(), "bounded child result");
    assert!(!dir.join("session.jsonl").exists());
    assert!(dir.join("session.redb").exists());
    drop(agent);
    server.join().unwrap();
    std::fs::remove_dir_all(root).unwrap();
}
