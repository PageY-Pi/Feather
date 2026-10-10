//! Isolated catalog + mock HTTP checks for explicit child options and timeout.
use feather::subagent::SubagentTool;
use feather_core::{
    agent::ProviderRef,
    provider::faux::{FauxProvider, FauxScript},
    substore::SubagentStore,
    tool::AgentTool,
};
use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::Arc,
};
#[tokio::test]
async fn model_thinking_empty_tools_progress_and_deadline() {
    let root = std::env::temp_dir().join(feather_core::provider::gen_id("child-options"));
    std::fs::create_dir_all(&root).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}/v1", listener.local_addr().unwrap());
    std::fs::write(root.join("models.json"),serde_json::to_vec(&serde_json::json!({"providers":{"test":{"apiKey":"synthetic-key","baseUrl":base,"api":"openai-completions","models":[{"id":"reasoner","reasoning":true,"thinkingLevels":["off","low","high"]}]}}})).unwrap()).unwrap();
    let old = std::env::var_os("FEATHER_HOME");
    std::env::set_var("FEATHER_HOME", &root);
    let server = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        let mut raw = Vec::new();
        let body: serde_json::Value = loop {
            let mut buf = [0; 4096];
            let n = socket.read(&mut buf).unwrap();
            assert!(n > 0);
            raw.extend_from_slice(&buf[..n]);
            if let Some(end) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&raw[..end]);
                let len: usize = headers
                    .lines()
                    .find_map(|l| {
                        l.split_once(':')
                            .filter(|(k, _)| k.eq_ignore_ascii_case("content-length"))
                            .map(|(_, v)| v.trim().parse().unwrap())
                    })
                    .unwrap();
                if raw.len() >= end + 4 + len {
                    break serde_json::from_slice(&raw[end + 4..end + 4 + len]).unwrap();
                }
            }
        };
        let data="data: {\"choices\":[{\"delta\":{\"content\":\"OK\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";
        std::thread::sleep(std::time::Duration::from_millis(200));
        write!(socket,"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",data.len(),data).unwrap();
        body
    });
    let child = SubagentTool {
        provider: Arc::new(ProviderRef::Faux(FauxProvider::new(FauxScript::new()))),
        root: root.clone(),
        session_dir: root.join("session"),
        model: "faux".into(),
        max_tokens: 32,
        context_window: 10000,
        bash_timeout: std::time::Duration::from_secs(1),
        inherited_tools: None,
        use_policy: false,
    };
    let (_tx, rx) = tokio::sync::watch::channel(false);
    let (progress_tx, mut progress_rx) = tokio::sync::mpsc::unbounded_channel();
    let out=child.execute_with_progress(serde_json::json!({"task":"synthetic","model":"test/reasoner","thinking":"high","tools":[]}),rx,progress_tx).await;
    assert!(!out.is_error, "{}", out.text);
    let body = server.join().unwrap();
    assert_eq!(body["model"], "reasoner");
    assert_eq!(body["reasoning_effort"], "high");
    assert_eq!(body["tools"], serde_json::json!([]));
    assert!(progress_rx.try_recv().is_ok());
    let store = SubagentStore::open(&root.join("session")).unwrap();
    assert_eq!(store.list().unwrap()[0].output.as_deref(), Some("OK"));
    drop(store);
    // A server which never sends headers must still respond to the overall deadline.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        let (_socket, _) = listener.accept().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(600));
    });
    let mut child = child;
    child.provider = Arc::new(ProviderRef::OpenAi(Arc::new(
        feather_core::OpenAiProvider::new("synthetic", Some(base)),
    )));
    let (_tx, rx) = tokio::sync::watch::channel(false);
    let started = std::time::Instant::now();
    let out = child
        .execute(
            serde_json::json!({"task":"synthetic","tools":[],"timeoutMs":100}),
            rx,
        )
        .await;
    assert!(
        out.is_error && out.text.contains("timed out"),
        "{}",
        out.text
    );
    assert!(started.elapsed() < std::time::Duration::from_millis(500));
    server.join().unwrap();
    match old {
        Some(value) => std::env::set_var("FEATHER_HOME", value),
        None => std::env::remove_var("FEATHER_HOME"),
    };
    std::fs::remove_dir_all(root).unwrap();
}
