use feather_core::{
    agent::{Agent, AgentConfig, ProviderRef},
    compaction::CompactionSettings,
    message::Api,
    provider::faux::{FauxProvider, FauxScript},
    session::Session,
    tool::{AgentTool, BoxFut, ToolOutcome, ToolRegistry},
};
use serde_json::{json, Value};
use std::{path::Path, sync::Arc, time::Duration};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

struct Hold;
impl AgentTool for Hold {
    fn name(&self) -> &'static str {
        "hold"
    }
    fn description(&self) -> &'static str {
        "wait until aborted"
    }
    fn parameters(&self) -> Value {
        json!({"type":"object"})
    }
    fn execute<'a>(
        &'a self,
        _: Value,
        mut abort: tokio::sync::watch::Receiver<bool>,
    ) -> BoxFut<'a> {
        Box::pin(async move {
            while !*abort.borrow_and_update() {
                if abort.changed().await.is_err() {
                    break;
                }
            }
            ToolOutcome::err("aborted")
        })
    }
}
fn agent(session: Session, hold: bool) -> Agent {
    let mut tools = ToolRegistry::new();
    tools.add(Arc::new(Hold));
    let script = if hold {
        FauxScript::new().with_tool("hold", json!({}))
    } else {
        FauxScript::new().with_text("Hello\u{2028}world")
    };
    Agent::new(
        AgentConfig {
            model: "faux".into(),
            api: Api::Faux,
            system_prompt: "test".into(),
            max_tokens: 1024,
            temperature: None,
            context_window: 200_000,
            thinking_level: None,
            compaction: CompactionSettings::default(),
            compaction_model: None,
            tools: tools.defs().into(),
        },
        session,
        Arc::new(tools),
        ProviderRef::Faux(FauxProvider::new(script)),
        None,
    )
}
fn temp() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(feather_core::provider::gen_id("rpc-test"));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}
async fn record(
    lines: &mut tokio::io::Lines<BufReader<tokio::io::ReadHalf<tokio::io::DuplexStream>>>,
) -> Value {
    let line = tokio::time::timeout(Duration::from_secs(5), lines.next_line())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    serde_json::from_str(&line).unwrap()
}
#[tokio::test]
async fn prompt_acceptance_stream_settlement_and_session_switch() {
    let root = temp();
    let session = Session::new(&root.join(".feather"), &root).unwrap();
    let id = session.id().to_string();
    let (client, server) = tokio::io::duplex(8192);
    let (server_in, server_out) = tokio::io::split(server);
    let (client_in, mut client_out) = tokio::io::split(client);
    let mut lines = BufReader::new(client_in).lines();
    let home = root.join(".feather");
    let server = feather::rpc::run(
        agent(session, false),
        BufReader::new(server_in),
        server_out,
        &home,
        &root,
        |s| Ok(agent(s, false)),
    );
    let client = async {
        client_out.write_all(b"not-json\n").await.unwrap();
        assert_eq!(record(&mut lines).await["command"], "parse");
        let command = json!({"id":"p1","type":"prompt","message":"hi\u{2028}there","images":[{"type":"image","mimeType":"image/png","data":"AAAA"}]}).to_string();
        // Exercise split records; Unicode separators must not split framing.
        client_out
            .write_all(&command.as_bytes()[..5])
            .await
            .unwrap();
        tokio::task::yield_now().await;
        client_out
            .write_all(format!("{}\r\n", &command[5..]).as_bytes())
            .await
            .unwrap();
        let accepted = record(&mut lines).await;
        assert_eq!(accepted["id"], "p1");
        assert_eq!(accepted["data"]["disposition"], "started");
        let mut saw_delta = false;
        let mut saw_end = false;
        loop {
            let r = record(&mut lines).await;
            if r["type"] == "message_update" {
                saw_delta |= r["assistantMessageEvent"]["type"] == "text_delta";
            }
            if r["type"] == "agent_end" {
                saw_end = true;
            }
            if r["type"] == "agent_settled" {
                break;
            }
        }
        assert!(saw_delta && saw_end);
        client_out
            .write_all(b"{\"type\":\"get_messages\"}\n")
            .await
            .unwrap();
        let messages = record(&mut lines).await;
        assert_eq!(
            messages["data"]["messages"][0]["content"][1]["mimeType"],
            "image/png"
        );
        client_out
            .write_all(b"{\"type\":\"new_session\"}\n{\"type\":\"get_state\"}\n")
            .await
            .unwrap();
        assert_eq!(record(&mut lines).await["success"], true);
        let state = record(&mut lines).await;
        assert_ne!(state["data"]["sessionId"], id);
        assert_eq!(state["data"]["messageCount"], 0);
        client_out.shutdown().await.unwrap();
    };
    let (_, result) = tokio::join!(client, server);
    result.unwrap();
    std::fs::remove_dir_all(root).unwrap();
}
#[tokio::test]
async fn abort_is_responsive_while_tool_owns_agent_and_waits_for_settlement() {
    let root = temp();
    let session = Session::new(&root.join(".feather"), &root).unwrap();
    let (client, server) = tokio::io::duplex(8192);
    let (server_in, server_out) = tokio::io::split(server);
    let (client_in, mut client_out) = tokio::io::split(client);
    let mut lines = BufReader::new(client_in).lines();
    let server = feather::rpc::run(
        agent(session, true),
        BufReader::new(server_in),
        server_out,
        Path::new(&root),
        &root,
        |s| Ok(agent(s, false)),
    );
    let client = async {
        client_out
            .write_all(b"{\"id\":\"run\",\"type\":\"prompt\",\"message\":\"hold\"}\n")
            .await
            .unwrap();
        while record(&mut lines).await["type"] != "tool_execution_start" {}
        client_out.write_all(b"{\"id\":\"busy\",\"type\":\"prompt\",\"message\":\"reject\"}\n{\"id\":\"queue\",\"type\":\"steer\",\"message\":\"direction\"}\n{\"id\":\"clear\",\"type\":\"clear_queue\"}\n{\"id\":\"cancel\",\"type\":\"abort\"}\n").await.unwrap();
        let busy = record(&mut lines).await;
        assert_eq!(busy["id"], "busy");
        assert_eq!(busy["success"], false);
        let queued = record(&mut lines).await;
        assert_eq!(queued["data"]["disposition"], "queued");
        let cleared = record(&mut lines).await;
        assert_eq!(cleared["data"]["steering"][0], "direction");
        let mut settled = false;
        loop {
            let r = record(&mut lines).await;
            if r["type"] == "agent_settled" {
                settled = true;
            }
            if r["id"] == "cancel" {
                assert!(settled);
                assert_eq!(r["success"], true);
                break;
            }
        }
        client_out.shutdown().await.unwrap();
    };
    let (_, result) = tokio::join!(client, server);
    result.unwrap();
    std::fs::remove_dir_all(root).unwrap();
}
