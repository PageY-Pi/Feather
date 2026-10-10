//! Protocol, durability and file-layout checks through the real child tool.
use feather::{config::Settings, subagent::SubagentTool, tasks::TaskStore, wiring};
use feather_core::{
    agent::ProviderRef,
    provider::{
        faux::{FauxProvider, FauxScript},
        openai::OpenAiProvider,
    },
    substore::SubagentStore,
    tool::AgentTool,
};
use std::{
    io::{Read, Write},
    net::TcpListener,
    path::{Path, PathBuf},
    sync::Arc,
};

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(feather_core::provider::gen_id("sub-runtime"));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn assert_no_jsonl(path: &Path) {
    for entry in std::fs::read_dir(path).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            assert_no_jsonl(&path);
        } else {
            assert_ne!(
                path.extension().and_then(|e| e.to_str()),
                Some("jsonl"),
                "unexpected child transcript: {}",
                path.display()
            );
        }
    }
}
fn tool(root: &Path, session: &Path, provider: ProviderRef) -> SubagentTool {
    SubagentTool {
        provider: Arc::new(provider),
        root: root.into(),
        session_dir: session.into(),
        model: "synthetic".into(),
        max_tokens: 32,
        context_window: 10000,
        bash_timeout: std::time::Duration::from_secs(2),
        inherited_tools: None,
        use_policy: false,
    }
}
fn server(payload: &'static str) -> (String, std::thread::JoinHandle<serde_json::Value>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}/v1", listener.local_addr().unwrap());
    let handle = std::thread::spawn(move || {
        listener.set_nonblocking(true).unwrap();
        let started = std::time::Instant::now();
        let mut socket = loop {
            match listener.accept() {
                Ok((socket, _)) => break socket,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(
                        started.elapsed() < std::time::Duration::from_secs(5),
                        "child made no request"
                    );
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                Err(e) => panic!("{e}"),
            }
        };
        socket.set_nonblocking(false).unwrap();
        socket
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        let mut raw = Vec::new();
        let body = loop {
            let mut bytes = [0; 4096];
            let n = socket.read(&mut bytes).unwrap();
            assert!(n > 0);
            raw.extend_from_slice(&bytes[..n]);
            if let Some(end) = raw.windows(4).position(|v| v == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&raw[..end]);
                let len: usize = headers
                    .lines()
                    .find_map(|line| {
                        line.split_once(':')
                            .filter(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                            .map(|(_, value)| value.trim().parse().unwrap())
                    })
                    .unwrap();
                if raw.len() >= end + 4 + len {
                    break serde_json::from_slice(&raw[end + 4..end + 4 + len]).unwrap();
                }
            }
        };
        write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", payload.len(), payload).unwrap();
        body
    });
    (base, handle)
}

#[tokio::test]
async fn child_wire_model_has_no_catalog_prefix_and_result_is_redb_only() {
    let temp = Temp::new();
    let root = temp.0.join("project");
    let session = temp.0.join("session");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&session).unwrap();
    let (base, handle) = server("data: {\"choices\":[{\"delta\":{\"content\":\"OK\"}}]}\n\ndata: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n");
    let provider = Arc::new(ProviderRef::OpenAi(Arc::new(OpenAiProvider::new(
        "synthetic-test-key",
        Some(base),
    ))));
    let mut settings = Settings::default();
    settings.model = "test-provider/glm-5.3-flash".into();
    settings.subagent.enabled = true;
    settings.subagent.use_policy = false;
    let registry = wiring::build_registry(
        &root,
        &session.join("anchors"),
        &settings,
        &Arc::new(TaskStore::open(&session)),
        &session,
        &provider,
    );
    let (_tx, rx) = tokio::sync::watch::channel(false);
    let out = registry
        .get("subagent")
        .unwrap()
        .execute(serde_json::json!({"prompt":"Reply only OK"}), rx)
        .await;
    assert!(!out.is_error, "{}", out.text);
    assert!(out.text.ends_with("\nOK"));
    let request = handle.join().unwrap();
    assert_eq!(request["model"], "glm-5.3-flash");
    assert_eq!(request["messages"][1]["content"], "Reply only OK");
    let store = SubagentStore::open(&session).unwrap();
    let records = store.list().unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].model, "glm-5.3-flash");
    assert_eq!(records[0].output.as_deref(), Some("OK"));
    assert_eq!(store.messages(&records[0].id).unwrap().len(), 2);
    drop(store);
    drop(registry);
    assert_no_jsonl(&temp.0);
    assert!(session.join("subagents.redb").exists());
}

#[tokio::test]
async fn child_stream_error_is_failure_in_parent_and_redb() {
    let temp = Temp::new();
    let session = temp.0.join("session");
    // Terminal quota fixture needs one HTTP response. Transient errors now
    // retry 3 times (covered with paused-time scripted provider tests).
    let (base, handle) = server("data: {\"error\":{\"message\":\"synthetic stream error: insufficient_quota\"}}\n\n");
    let tool = tool(
        &temp.0,
        &session,
        ProviderRef::OpenAi(Arc::new(OpenAiProvider::new("synthetic-key", Some(base)))),
    );
    let (_tx, rx) = tokio::sync::watch::channel(false);
    let out = tool
        .execute(serde_json::json!({"prompt":"synthetic"}), rx)
        .await;
    assert!(out.is_error, "{}", out.text);
    assert!(out.text.contains("provider stream error"), "{}", out.text);
    handle.join().unwrap();
    let store = SubagentStore::open(&session).unwrap();
    let records = store.list().unwrap();
    assert!(records[0].error.is_some());
    assert!(records[0].output.is_none());
    assert!(records[0].finished_ms.is_some());
    assert_eq!(store.messages(&records[0].id).unwrap().len(), 1);
    drop(store);
    assert_no_jsonl(&temp.0);
}

#[tokio::test]
async fn child_tool_roundtrip_and_repeated_runs_are_recoverable_without_jsonl() {
    let temp = Temp::new();
    let session = temp.0.join("session");
    let provider = ProviderRef::Faux(FauxProvider::new(FauxScript::new().with_tool("write", serde_json::json!({"path": temp.0.join("child.txt"), "content":"synthetic child file"})).with_text("written").with_text("second")));
    let tool = tool(&temp.0, &session, provider);
    for expected in ["written", "second"] {
        let (_tx, rx) = tokio::sync::watch::channel(false);
        let out = tool
            .execute(serde_json::json!({"prompt":"synthetic task"}), rx)
            .await;
        assert!(!out.is_error, "{}", out.text);
        assert!(out.text.ends_with(expected));
    }
    let store = SubagentStore::open(&session).unwrap();
    let records = store.list().unwrap();
    assert_eq!(records.len(), 2);
    let first = records
        .iter()
        .find(|r| r.output.as_deref() == Some("written"))
        .unwrap();
    let messages = store.messages(&first.id).unwrap();
    assert_eq!(messages.len(), 4);
    assert_eq!(messages[2].role, feather_core::message::Role::ToolResult);
    assert!(!messages[2].is_error);
    assert_eq!(
        std::fs::read_to_string(temp.0.join("child.txt")).unwrap(),
        "synthetic child file"
    );
    drop(store);
    let reopened = SubagentStore::open(&session).unwrap();
    assert_eq!(reopened.list().unwrap().len(), 2);
    drop(reopened);
    assert_no_jsonl(&temp.0);
}

#[tokio::test]
async fn concurrent_children_share_redb_without_id_or_anchor_collisions() {
    let temp = Temp::new();
    let session = temp.0.join("session");
    let child = tool(
        &temp.0,
        &session,
        ProviderRef::Faux(FauxProvider::new(
            FauxScript::new().with_text("one").with_text("two"),
        )),
    );
    let (_tx, rx) = tokio::sync::watch::channel(false);
    let (a, b) = tokio::join!(
        child.execute(serde_json::json!({"task":"a","tools":[]}), rx.clone()),
        child.execute(serde_json::json!({"task":"b","tools":[]}), rx)
    );
    assert!(!a.is_error, "{}", a.text);
    assert!(!b.is_error, "{}", b.text);
    let store = SubagentStore::open(&session).unwrap();
    let records = store.list().unwrap();
    assert_eq!(records.len(), 2);
    assert_ne!(records[0].id, records[1].id);
    for record in records {
        assert_eq!(store.messages(&record.id).unwrap().len(), 2);
    }
    drop(store);
    assert_no_jsonl(&temp.0);
}

#[tokio::test]
async fn long_child_answer_is_bounded_but_full_output_stays_in_redb() {
    let temp = Temp::new();
    let session = temp.0.join("session");
    let answer = "羽毛\n".repeat(5000);
    let child = tool(
        &temp.0,
        &session,
        ProviderRef::Faux(FauxProvider::new(
            FauxScript::new().with_text(answer.clone()),
        )),
    );
    let (_tx, rx) = tokio::sync::watch::channel(false);
    let out = child
        .execute(serde_json::json!({"task":"synthetic","tools":[]}), rx)
        .await;
    assert!(!out.is_error, "{}", out.text);
    assert!(out.text.len() < 9000);
    assert!(out.text.contains("Truncated"));
    let store = SubagentStore::open(&session).unwrap();
    assert_eq!(
        store.list().unwrap()[0].output.as_deref(),
        Some(answer.as_str())
    );
    drop(store);
    assert_no_jsonl(&temp.0);
}

#[tokio::test]
async fn cancelled_child_preserves_prompt_and_error_in_redb_without_jsonl() {
    let temp = Temp::new();
    let session = temp.0.join("session");
    let tool = tool(
        &temp.0,
        &session,
        ProviderRef::Faux(FauxProvider::new(
            FauxScript::new().with_text("must not run"),
        )),
    );
    let (_tx, rx) = tokio::sync::watch::channel(true);
    let out = tool
        .execute(serde_json::json!({"prompt":"cancelled synthetic task"}), rx)
        .await;
    assert!(out.is_error);
    let store = SubagentStore::open(&session).unwrap();
    let records = store.list().unwrap();
    assert_eq!(records.len(), 1);
    assert!(records[0].error.as_deref().unwrap().contains("aborted"));
    assert!(records[0].output.is_none());
    let messages = store.messages(&records[0].id).unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].text(), "cancelled synthetic task");
    drop(store);
    assert_no_jsonl(&temp.0);
}
