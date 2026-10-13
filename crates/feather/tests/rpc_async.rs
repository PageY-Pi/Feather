//! Focused RPC tests for the async/cached behavior added on top of the
//! existing `rpc_e2e`:
//!
//! * `get_messages`/`get_session_stats` answer from the busy snapshot while a
//!   tool owns the agent, and queued steer images are echoed as messages.
//! * `compact` runs off the actor loop, reports `compaction_start`/`_end`, and
//!   `abort_compaction` is idle-safe.
//! * `run_with_control` dispatches `set_model`/`cycle_model` to the parent
//!   callback only while idle and `Agent::reconfigure` retargets the provider.

use feather_core::{
    agent::{Agent, AgentConfig, ProviderRef},
    compaction::CompactionSettings,
    message::{Api, ContentBlock, Message, StopReason, Usage},
    provider::faux::{FauxProvider, FauxScript},
    session::Session,
    tool::{AgentTool, BoxFut, ToolOutcome, ToolRegistry},
};
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::Semaphore;

/// A tool that blocks until the test hands it a semaphore permit. This keeps a
/// run "busy" so the actor loop must answer commands without owning the agent.
struct Gate {
    permit: Arc<Semaphore>,
}
impl AgentTool for Gate {
    fn name(&self) -> &'static str {
        "gate"
    }
    fn description(&self) -> &'static str {
        "block until released"
    }
    fn parameters(&self) -> Value {
        json!({"type":"object"})
    }
    fn execute<'a>(&'a self, _: Value, _abort: tokio::sync::watch::Receiver<bool>) -> BoxFut<'a> {
        let permit = self.permit.clone();
        Box::pin(async move {
            let _ = permit.acquire().await;
            ToolOutcome::ok("released")
        })
    }
}

fn registry(permit: Arc<Semaphore>) -> Arc<ToolRegistry> {
    let mut tools = ToolRegistry::new();
    tools.add(Arc::new(Gate { permit }));
    Arc::new(tools)
}

fn base_config(model: &str) -> AgentConfig {
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

fn agent(session: Session, tools: Arc<ToolRegistry>, script: FauxScript) -> Agent {
    Agent::new(
        AgentConfig {
            tools: tools.defs().into(),
            ..base_config("faux")
        },
        session,
        tools,
        ProviderRef::Faux(FauxProvider::new(script)),
        None,
    )
}

fn temp(tag: &str) -> std::path::PathBuf {
    let dir =
        std::env::temp_dir().join(feather_core::provider::gen_id(&format!("rpc-async-{tag}")));
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
async fn busy_get_messages_stats_and_queued_images_are_responsive() {
    let root = temp("busy");
    let session = Session::new(&root.join(".feather"), &root).unwrap();
    let permit = Arc::new(Semaphore::new(0));
    let tools = registry(permit.clone());
    let script = FauxScript::new()
        .with_tool("gate", json!({}))
        .with_text("done");

    let (client, server) = tokio::io::duplex(65_536);
    let (server_in, server_out) = tokio::io::split(server);
    let (client_in, mut client_out) = tokio::io::split(client);
    let mut lines = BufReader::new(client_in).lines();
    let home = root.join(".feather");
    let server = feather::rpc::run(
        agent(session, tools.clone(), script),
        BufReader::new(server_in),
        server_out,
        &home,
        &root,
        move |s| {
            Ok(agent(
                s,
                registry(Arc::new(Semaphore::new(0))),
                FauxScript::new().with_text("x"),
            ))
        },
    );

    let client = async {
        client_out
            .write_all(b"{\"id\":\"run\",\"type\":\"prompt\",\"message\":\"go\"}\n")
            .await
            .unwrap();
        while record(&mut lines).await["type"] != "tool_execution_start" {}

        let steer = json!({
            "id":"steer","type":"steer","message":"look",
            "images":[{"type":"image","mimeType":"image/png","data":"AAAA"}]
        })
        .to_string();
        client_out
            .write_all(
                format!(
                    "{{\"id\":\"msgs\",\"type\":\"get_messages\"}}\n{steer}\n{{\"id\":\"stats\",\"type\":\"get_session_stats\"}}\n"
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        // responses land in command order while the run is parked in the gate
        let msgs = record(&mut lines).await;
        assert_eq!(msgs["id"], "msgs");
        assert_eq!(
            msgs["success"], true,
            "get_messages answers while busy: {msgs}"
        );
        assert!(msgs["data"]["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["role"] == "user" && m["content"][0]["text"] == "go"));

        let steer_response = record(&mut lines).await;
        assert_eq!(steer_response["id"], "steer");
        assert_eq!(steer_response["data"]["disposition"], "queued");

        let stats = record(&mut lines).await;
        assert_eq!(stats["id"], "stats");
        assert_eq!(
            stats["success"], true,
            "get_session_stats answers while busy: {stats}"
        );
        assert_eq!(
            stats["data"]["toolCalls"], 1,
            "the blocked assistant tool call is counted"
        );

        // release the gate so the queued steer is delivered and the run settles
        permit.add_permits(1);
        let mut records = Vec::new();
        loop {
            let r = record(&mut lines).await;
            let settled = r["type"] == "agent_settled";
            records.push(r);
            if settled {
                break;
            }
        }

        let echoed = records.iter().any(|r| {
            r["type"] == "message_end"
                && r["message"]["role"] == "user"
                && r["message"]["content"]
                    .as_array()
                    .map(|blocks| blocks.iter().any(|b| b["mimeType"] == "image/png"))
                    .unwrap_or(false)
        });
        assert!(echoed, "queued steer image was echoed on delivery");

        client_out.shutdown().await.unwrap();
    };

    let (_, result) = tokio::join!(client, server);
    result.unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn compact_command_is_async_and_abort_compaction_is_idle_safe() {
    let root = temp("compact");
    let mut session = Session::new(&root.join(".feather"), &root).unwrap();
    for i in 0..3 {
        session
            .push(Message::user_text(format!(
                "turn {i} {}",
                "x".repeat(4_000)
            )))
            .unwrap();
        session
            .push(Message::assistant(
                vec![ContentBlock::Text {
                    text: "answer".into(),
                }],
                "faux",
                Api::Faux,
                Usage::default(),
                StopReason::Stop,
            ))
            .unwrap();
        session.commit_turn().unwrap();
    }

    let settings = CompactionSettings {
        enabled: true,
        reserve_tokens: 16_384,
        keep_recent_tokens: 1,
        summary_tool_result_budget: 2_000,
        kept_tool_result_budget: 4_000,
        shrink_kept: true,
        keep_last_turns_unshrunk: 0,
    };
    let tools = Arc::new(ToolRegistry::new());
    let starting = Agent::new(
        AgentConfig {
            compaction: settings,
            ..base_config("faux")
        },
        session,
        tools.clone(),
        ProviderRef::Faux(FauxProvider::new(
            FauxScript::new().with_text("summary text"),
        )),
        None,
    );

    let (client, server) = tokio::io::duplex(65_536);
    let (server_in, server_out) = tokio::io::split(server);
    let (client_in, mut client_out) = tokio::io::split(client);
    let mut lines = BufReader::new(client_in).lines();
    let home = root.join(".feather");
    let server = feather::rpc::run(
        starting,
        BufReader::new(server_in),
        server_out,
        &home,
        &root,
        |s| Ok(agent(s, tools.clone(), FauxScript::new().with_text("x"))),
    );

    let client = async {
        client_out
            .write_all(b"{\"id\":\"c\",\"type\":\"compact\"}\n")
            .await
            .unwrap();
        let mut records = Vec::new();
        loop {
            let r = record(&mut lines).await;
            let settled = r["type"] == "agent_settled";
            records.push(r);
            if settled {
                break;
            }
        }
        assert!(records.iter().any(|r| r["type"] == "compaction_start"));
        let response = records
            .iter()
            .find(|r| r["id"] == "c")
            .expect("compact response");
        assert_eq!(response["success"], true);
        let end = records
            .iter()
            .find(|r| r["type"] == "compaction_end")
            .expect("compaction_end");
        assert_eq!(end["reason"], "manual");
        assert_eq!(end["aborted"], false);
        assert!(end["result"]["tokensBefore"].as_u64().unwrap() > 0);

        // idle abort_compaction is a no-op success
        client_out
            .write_all(b"{\"id\":\"ac\",\"type\":\"abort_compaction\"}\n")
            .await
            .unwrap();
        let idle = record(&mut lines).await;
        assert_eq!(idle["id"], "ac");
        assert_eq!(idle["success"], true);

        client_out.shutdown().await.unwrap();
    };

    let (_, result) = tokio::join!(client, server);
    result.unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn compact_same_length_replacement_refreshes_busy_snapshot() {
    let root = temp("compact-same-length");
    let mut session = Session::new(&root.join(".feather"), &root).unwrap();
    session
        .push(Message::user_text(format!(
            "old prefix {}",
            "x".repeat(4_000)
        )))
        .unwrap();
    session.push(Message::user_text("kept next user")).unwrap();
    session
        .push(Message::assistant(
            vec![ContentBlock::Text {
                text: "final answer".into(),
            }],
            "faux",
            Api::Faux,
            Usage::default(),
            StopReason::Stop,
        ))
        .unwrap();
    session.commit_turn().unwrap();

    let settings = CompactionSettings {
        enabled: true,
        reserve_tokens: 16_384,
        keep_recent_tokens: 1,
        summary_tool_result_budget: 2_000,
        kept_tool_result_budget: 4_000,
        shrink_kept: true,
        keep_last_turns_unshrunk: 0,
    };
    let permit = Arc::new(Semaphore::new(0));
    let tools = registry(permit.clone());
    let starting = Agent::new(
        AgentConfig {
            tools: tools.defs().into(),
            compaction: settings,
            ..base_config("faux")
        },
        session,
        tools.clone(),
        ProviderRef::Faux(FauxProvider::new(
            FauxScript::new()
                .with_text("summary text")
                .with_tool("gate", json!({}))
                .with_text("done"),
        )),
        None,
    );

    let (client, server) = tokio::io::duplex(65_536);
    let (server_in, server_out) = tokio::io::split(server);
    let (client_in, mut client_out) = tokio::io::split(client);
    let mut lines = BufReader::new(client_in).lines();
    let home = root.join(".feather");
    let server = feather::rpc::run(
        starting,
        BufReader::new(server_in),
        server_out,
        &home,
        &root,
        move |s| Ok(agent(s, tools.clone(), FauxScript::new().with_text("x"))),
    );

    let client = async {
        client_out
            .write_all(b"{\"id\":\"c\",\"type\":\"compact\"}\n")
            .await
            .unwrap();
        let mut records = Vec::new();
        loop {
            let r = record(&mut lines).await;
            let settled = r["type"] == "agent_settled";
            records.push(r);
            if settled {
                break;
            }
        }
        assert!(records.iter().any(|r| r["type"] == "compaction_start"));
        assert!(records.iter().any(|r| r["type"] == "compaction_end"));
        assert!(records
            .iter()
            .any(|r| r["id"] == "c" && r["success"] == true));

        let end = records
            .iter()
            .find(|r| r["type"] == "compaction_end")
            .expect("compaction_end");
        assert!(
            end["result"]["summary"]
                .as_str()
                .unwrap()
                .contains("summary text"),
            "the summary is carried by compaction_end: {end}"
        );
        let leaked = leaked_summary_records(&records, "summary text");
        assert!(
            leaked.is_empty(),
            "manual compaction streamed the summary as chat messages: {leaked:?}"
        );
        client_out
            .write_all(b"{\"id\":\"run\",\"type\":\"prompt\",\"message\":\"block\"}\n")
            .await
            .unwrap();
        loop {
            if record(&mut lines).await["type"] == "tool_execution_start" {
                break;
            }
        }

        client_out
            .write_all(b"{\"id\":\"msgs\",\"type\":\"get_messages\"}\n")
            .await
            .unwrap();
        let msgs = record(&mut lines).await;
        assert_eq!(msgs["id"], "msgs");
        assert_eq!(msgs["success"], true);
        let messages = msgs["data"]["messages"].as_array().unwrap();
        assert_eq!(
            messages.len(),
            5,
            "compacted 3 messages plus active prompt/tool call: {messages:?}"
        );
        let first = messages[0]["content"][0]["text"].as_str().unwrap();
        assert!(
            first.starts_with("## Conversation summary (feather compaction)")
                && first.contains("summary text"),
            "first message is refreshed compaction summary: {first:?}"
        );
        assert_eq!(messages[1]["content"][0]["text"], "kept next user");
        assert_eq!(messages[2]["role"], "assistant");
        assert_eq!(messages[2]["content"][0]["text"], "final answer");
        assert_eq!(messages[3]["content"][0]["text"], "block");

        permit.add_permits(1);
        loop {
            if record(&mut lines).await["type"] == "agent_settled" {
                break;
            }
        }
        // the settled tail is the run's own answer, never the summary output
        client_out
            .write_all(b"{\"id\":\"last\",\"type\":\"get_last_assistant_text\"}\n")
            .await
            .unwrap();
        let last = record(&mut lines).await;
        assert_eq!(last["id"], "last");
        let text = last["data"]["text"].as_str().unwrap_or("");
        assert_eq!(text, "done", "settled tail is the run's answer: {last}");
        assert!(
            !text.contains("summary text"),
            "compaction summary surfaced as assistant text: {last}"
        );
        assert!(
            !records.iter().any(|r| matches!(
                r["type"].as_str(),
                Some("message_update" | "message_start" | "message_end")
            )),
            "manual compaction summary leaked as normal message events: {records:?}"
        );
        client_out.shutdown().await.unwrap();
    };

    let (_, result) = tokio::join!(client, server);
    result.unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

/// Records that carry the summarization output as if it were a normal chat
/// message. A manual compaction must surface its summary only through
/// `compaction_end`/`get_messages`, never as streamed assistant traffic.
fn leaked_summary_records(records: &[Value], marker: &str) -> Vec<Value> {
    records
        .iter()
        .filter(|r| {
            matches!(
                r["type"].as_str(),
                Some("message_start" | "message_update" | "message_end" | "turn_end" | "agent_end")
            )
        })
        .filter(|r| r.to_string().contains(marker))
        .cloned()
        .collect()
}

/// Manual compaction that rewrites only the oldest user message keeps the
/// message *count* unchanged, so the busy snapshot must refresh on the changed
/// prefix too (not just on `len`). Also guards against the summarization turn
/// leaking as normal `message_*` events.
#[tokio::test]
async fn manual_compaction_replaces_prefix_with_summary_without_stream_leak() {
    let root = temp("compact-prefix-summary");
    let mut session = Session::new(&root.join(".feather"), &root).unwrap();
    session
        .push(Message::user_text(format!(
            "old prefix {}",
            "x".repeat(4_000)
        )))
        .unwrap();
    session.push(Message::user_text("user recent")).unwrap();
    session
        .push(Message::assistant(
            vec![ContentBlock::Text {
                text: "FINAL PURE TEXT".into(),
            }],
            "faux",
            Api::Faux,
            Usage::default(),
            StopReason::Stop,
        ))
        .unwrap();
    session.commit_turn().unwrap();
    assert_eq!(session.messages().len(), 3, "seeded: user, user, assistant");

    let settings = CompactionSettings {
        enabled: true,
        reserve_tokens: 16_384,
        keep_recent_tokens: 1,
        summary_tool_result_budget: 2_000,
        kept_tool_result_budget: 4_000,
        shrink_kept: true,
        keep_last_turns_unshrunk: 0,
    };
    let permit = Arc::new(Semaphore::new(0));
    let tools = registry(permit.clone());
    let starting = Agent::new(
        AgentConfig {
            tools: tools.defs().into(),
            compaction: settings,
            ..base_config("faux")
        },
        session,
        tools.clone(),
        ProviderRef::Faux(FauxProvider::new(
            FauxScript::new()
                .with_text("summary text")
                .with_tool("gate", json!({}))
                .with_text("done"),
        )),
        None,
    );

    let (client, server) = tokio::io::duplex(65_536);
    let (server_in, server_out) = tokio::io::split(server);
    let (client_in, mut client_out) = tokio::io::split(client);
    let mut lines = BufReader::new(client_in).lines();
    let home = root.join(".feather");
    let server = feather::rpc::run(
        starting,
        BufReader::new(server_in),
        server_out,
        &home,
        &root,
        move |s| Ok(agent(s, tools.clone(), FauxScript::new().with_text("x"))),
    );

    let client = async {
        client_out
            .write_all(b"{\"id\":\"c\",\"type\":\"compact\"}\n")
            .await
            .unwrap();
        let mut records = Vec::new();
        loop {
            let r = record(&mut lines).await;
            let settled = r["type"] == "agent_settled";
            records.push(r);
            if settled {
                break;
            }
        }
        assert!(records.iter().any(|r| r["type"] == "compaction_start"));
        let response = records
            .iter()
            .find(|r| r["id"] == "c")
            .expect("compact response");
        assert_eq!(response["success"], true, "compact failed: {response}");
        let end = records
            .iter()
            .find(|r| r["type"] == "compaction_end")
            .expect("compaction_end");
        assert_eq!(end["reason"], "manual");
        assert_eq!(end["aborted"], false);
        let summary = end["result"]["summary"].as_str().unwrap();
        assert!(
            summary.contains("summary text"),
            "summary is reported on compaction_end: {summary:?}"
        );
        assert!(end["result"]["tokensBefore"].as_u64().unwrap() > 0);

        // the summarization turn is not chat traffic
        let leaked = leaked_summary_records(&records, "summary text");
        assert!(
            leaked.is_empty(),
            "summary leaked as message events: {leaked:?}"
        );
        assert!(
            !records.iter().any(|r| matches!(
                r["type"].as_str(),
                Some("message_start" | "message_update" | "message_end")
            )),
            "manual compaction emitted message events: {records:?}"
        );

        // same count: the prefix message was swapped for the summary
        client_out
            .write_all(b"{\"id\":\"m1\",\"type\":\"get_messages\"}\n")
            .await
            .unwrap();
        let msgs = record(&mut lines).await;
        assert_eq!(msgs["id"], "m1");
        assert_eq!(msgs["success"], true);
        let messages = msgs["data"]["messages"].as_array().unwrap().clone();
        assert_eq!(messages.len(), 3, "compaction kept the count: {messages:?}");
        let first = messages[0]["content"][0]["text"].as_str().unwrap();
        assert!(
            first.starts_with("## Conversation summary (feather compaction)")
                && first.contains("summary text"),
            "first message is the summary: {first:?}"
        );
        assert_eq!(messages[1]["content"][0]["text"], "user recent");
        assert_eq!(messages[2]["role"], "assistant");
        assert_eq!(messages[2]["content"][0]["text"], "FINAL PURE TEXT");

        // a later run must refresh the cached snapshot, not keep the stale prefix
        client_out
            .write_all(b"{\"id\":\"run\",\"type\":\"prompt\",\"message\":\"block\"}\n")
            .await
            .unwrap();
        while record(&mut lines).await["type"] != "tool_execution_start" {}
        client_out
            .write_all(b"{\"id\":\"m2\",\"type\":\"get_messages\"}\n")
            .await
            .unwrap();
        let busy = record(&mut lines).await;
        assert_eq!(busy["id"], "m2");
        assert_eq!(busy["success"], true);
        let busy_messages = busy["data"]["messages"].as_array().unwrap().clone();
        assert_eq!(
            busy_messages.len(),
            5,
            "compacted 3 plus the active prompt and its tool call: {busy_messages:?}"
        );
        let busy_first = busy_messages[0]["content"][0]["text"].as_str().unwrap();
        let busy_head: String = busy_first.chars().take(60).collect();
        assert!(
            busy_first.starts_with("## Conversation summary (feather compaction)"),
            "busy snapshot kept the pre-compaction prefix: {busy_head:?}"
        );
        assert!(
            !busy_first.contains("old prefix"),
            "busy snapshot still holds the summarized message: {busy_head:?}"
        );
        assert_eq!(busy_messages[2]["content"][0]["text"], "FINAL PURE TEXT");
        assert_eq!(busy_messages[3]["content"][0]["text"], "block");
        assert_eq!(busy_messages[4]["role"], "assistant");
        assert_eq!(
            busy_messages[4]["content"][0]["name"], "gate",
            "the blocked tool call is the newest message: {busy_messages:?}"
        );

        permit.add_permits(1);
        loop {
            if record(&mut lines).await["type"] == "agent_settled" {
                break;
            }
        }
        assert!(
            leaked_summary_records(&records, "summary text").is_empty(),
            "summary leaked into the follow-up run: {records:?}"
        );
        client_out.shutdown().await.unwrap();
    };

    let (_, result) = tokio::join!(client, server);
    result.unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn model_control_runs_idle_but_is_rejected_while_busy() {
    let root = temp("control");
    let session = Session::new(&root.join(".feather"), &root).unwrap();
    let permit = Arc::new(Semaphore::new(0));
    let tools = registry(permit.clone());
    let script = FauxScript::new()
        .with_tool("gate", json!({}))
        .with_text("done");

    let calls = Arc::new(AtomicUsize::new(0));
    let calls2 = calls.clone();
    let switched_provider = ProviderRef::Faux(FauxProvider::new(
        FauxScript::new()
            .with_tool("gate", json!({}))
            .with_text("switched"),
    ));
    let switched_tools = tools.clone();
    let factory_tools = switched_tools.clone();
    let mut switched_config = base_config("faux-switched");
    switched_config.tools = switched_tools.defs().into();

    let control = move |agent: &mut Agent, command: &Value| -> Result<Value, String> {
        calls2.fetch_add(1, Ordering::SeqCst);
        match command["type"].as_str().unwrap_or("") {
            "set_model" => {
                let provider = command["provider"].as_str().unwrap_or("");
                let model_id = command["modelId"].as_str().unwrap_or("");
                if provider != "faux" || model_id != "faux-switched" {
                    return Err(format!("Model not found: {provider}/{model_id}"));
                }
                agent.reconfigure(
                    switched_config.clone(),
                    switched_provider.clone(),
                    switched_tools.clone(),
                );
                Ok(json!({"id":"faux-switched","provider":"faux","api":"faux"}))
            }
            "cycle_model" => {
                // the parent parses provider/modelId + direction out of the command
                let direction = command["direction"].as_str().unwrap_or("forward");
                Ok(json!({
                    "model":{"id":"faux-switched","provider":"faux","api":"faux"},
                    "thinkingLevel":"medium",
                    "isScoped":false,
                    "direction":direction
                }))
            }
            other => Err(format!("Unsupported command: {other}")),
        }
    };

    let (client, server) = tokio::io::duplex(65_536);
    let (server_in, server_out) = tokio::io::split(server);
    let (client_in, mut client_out) = tokio::io::split(client);
    let mut lines = BufReader::new(client_in).lines();
    let home = root.join(".feather");
    let server = feather::rpc::run_with_control(
        agent(session, tools.clone(), script),
        BufReader::new(server_in),
        server_out,
        &home,
        &root,
        move |s| {
            Ok(agent(
                s,
                factory_tools.clone(),
                FauxScript::new().with_text("x"),
            ))
        },
        control,
    );

    let client = async {
        // idle: the parent callback runs and the model actually switches
        client_out
            .write_all(
                b"{\"id\":\"sm\",\"type\":\"set_model\",\"provider\":\"faux\",\"modelId\":\"faux-switched\"}\n{\"id\":\"st\",\"type\":\"get_state\"}\n",
            )
            .await
            .unwrap();
        let sm = record(&mut lines).await;
        assert_eq!(sm["id"], "sm");
        assert_eq!(sm["success"], true);
        assert_eq!(sm["data"]["id"], "faux-switched");
        let state = record(&mut lines).await;
        assert_eq!(state["data"]["model"]["id"], "faux-switched");

        // busy: rejected before the callback is ever consulted
        client_out
            .write_all(b"{\"id\":\"run\",\"type\":\"prompt\",\"message\":\"go\"}\n")
            .await
            .unwrap();
        while record(&mut lines).await["type"] != "tool_execution_start" {}
        client_out
            .write_all(
                b"{\"id\":\"busy\",\"type\":\"set_model\",\"provider\":\"faux\",\"modelId\":\"faux-switched\"}\n",
            )
            .await
            .unwrap();
        let busy = record(&mut lines).await;
        assert_eq!(busy["id"], "busy");
        assert_eq!(busy["success"], false);

        permit.add_permits(1);
        loop {
            if record(&mut lines).await["type"] == "agent_settled" {
                break;
            }
        }
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "control was not invoked while busy"
        );

        client_out.shutdown().await.unwrap();
    };

    let (_, result) = tokio::join!(client, server);
    result.unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn bash_streams_updates_and_returns_pi_result() {
    let root = temp("bash");
    let session = Session::new(&root.join(".feather"), &root).unwrap();
    let tools = Arc::new(ToolRegistry::new());
    let (client, server) = tokio::io::duplex(65_536);
    let (server_in, server_out) = tokio::io::split(server);
    let (client_in, mut client_out) = tokio::io::split(client);
    let mut lines = BufReader::new(client_in).lines();
    let home = root.join(".feather");
    let server = feather::rpc::run(
        agent(session, tools.clone(), FauxScript::new().with_text("x")),
        BufReader::new(server_in),
        server_out,
        &home,
        &root,
        move |s| Ok(agent(s, tools.clone(), FauxScript::new().with_text("x"))),
    );

    let client = async {
        client_out
            .write_all(b"{\"id\":\"b1\",\"type\":\"bash\",\"command\":\"echo feather-bash\"}\n")
            .await
            .unwrap();
        let mut updates = 0usize;
        let response = loop {
            let r = record(&mut lines).await;
            if r["type"] == "bash_execution_update" {
                assert_eq!(r["id"], "b1");
                assert!(r["delta"].is_string());
                updates += 1;
            }
            if r["type"] == "response" && r["id"] == "b1" {
                break r;
            }
        };
        assert_eq!(response["success"], true);
        assert_eq!(response["data"]["exitCode"], 0);
        assert_eq!(response["data"]["cancelled"], false);
        assert_eq!(response["data"]["truncated"], false);
        assert!(response["data"]["output"]
            .as_str()
            .unwrap()
            .contains("feather-bash"));
        assert!(updates >= 1, "bash streamed at least one update");

        // completed output becomes model context on the next prompt
        client_out
            .write_all(b"{\"id\":\"m1\",\"type\":\"get_messages\"}\n")
            .await
            .unwrap();
        let msgs = loop {
            let r = record(&mut lines).await;
            if r["id"] == "m1" {
                break r;
            }
        };
        assert!(msgs["data"]["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| {
                m["role"] == "user"
                    && m["content"][0]["text"]
                        .as_str()
                        .unwrap_or("")
                        .starts_with("Ran `echo feather-bash`")
            }));
        client_out.shutdown().await.unwrap();
    };

    let (_, result) = tokio::join!(client, server);
    result.unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn bash_preserves_utf8_split_across_chunks() {
    let root = temp("bash-unicode");
    let session = Session::new(&root.join(".feather"), &root).unwrap();
    let tools = Arc::new(ToolRegistry::new());
    let (client, server) = tokio::io::duplex(65_536);
    let (server_in, server_out) = tokio::io::split(server);
    let (client_in, mut client_out) = tokio::io::split(client);
    let mut lines = BufReader::new(client_in).lines();
    let home = root.join(".feather");
    let server = feather::rpc::run(
        agent(session, tools.clone(), FauxScript::new().with_text("x")),
        BufReader::new(server_in),
        server_out,
        &home,
        &root,
        move |s| Ok(agent(s, tools.clone(), FauxScript::new().with_text("x"))),
    );

    let client = async {
        client_out
            .write_all(
                b"{\"id\":\"u1\",\"type\":\"bash\",\"command\":\"printf '\\\\344\\\\270'; sleep 0.05; printf '\\\\255\xE6\x96\x87\\\\n'\"}\n",
            )
            .await
            .unwrap();
        let response = loop {
            let r = record(&mut lines).await;
            if r["type"] == "response" && r["id"] == "u1" {
                break r;
            }
        };
        assert_eq!(response["success"], true);
        assert_eq!(response["data"]["output"], "中文\n");
        client_out.shutdown().await.unwrap();
    };

    let (_, result) = tokio::join!(client, server);
    result.unwrap();
    std::fs::remove_dir_all(root).unwrap();
}
#[tokio::test]
async fn abort_bash_cancels_a_running_command() {
    let root = temp("abort-bash");
    let session = Session::new(&root.join(".feather"), &root).unwrap();
    let tools = Arc::new(ToolRegistry::new());
    let (client, server) = tokio::io::duplex(65_536);
    let (server_in, server_out) = tokio::io::split(server);
    let (client_in, mut client_out) = tokio::io::split(client);
    let mut lines = BufReader::new(client_in).lines();
    let home = root.join(".feather");
    let server = feather::rpc::run(
        agent(session, tools.clone(), FauxScript::new().with_text("x")),
        BufReader::new(server_in),
        server_out,
        &home,
        &root,
        move |s| Ok(agent(s, tools.clone(), FauxScript::new().with_text("x"))),
    );

    let client = async {
        let command = "sleep 30";
        client_out
            .write_all(
                format!("{{\"id\":\"b1\",\"type\":\"bash\",\"command\":\"{command}\"}}\n")
                    .as_bytes(),
            )
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(400)).await;
        client_out
            .write_all(b"{\"id\":\"a1\",\"type\":\"abort_bash\"}\n")
            .await
            .unwrap();
        let abort = loop {
            let r = record(&mut lines).await;
            if r["id"] == "a1" {
                break r;
            }
        };
        assert_eq!(abort["success"], true);
        let response = loop {
            let r = record(&mut lines).await;
            if r["type"] == "response" && r["id"] == "b1" {
                break r;
            }
        };
        assert_eq!(response["success"], true);
        assert_eq!(response["data"]["cancelled"], true);
        assert!(response["data"]["exitCode"].is_null());
        client_out.shutdown().await.unwrap();
    };

    let (_, result) = tokio::join!(client, server);
    result.unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn session_name_entries_and_fork_messages() {
    let root = temp("entries");
    let mut session = Session::new(&root.join(".feather"), &root).unwrap();
    session.push(Message::user_text("first prompt")).unwrap();
    session.commit_turn().unwrap();
    session
        .push(Message::assistant(
            vec![ContentBlock::Text {
                text: "answer".into(),
            }],
            "faux",
            Api::Faux,
            Usage::default(),
            StopReason::Stop,
        ))
        .unwrap();
    session.commit_turn().unwrap();
    let tools = Arc::new(ToolRegistry::new());
    let (client, server) = tokio::io::duplex(65_536);
    let (server_in, server_out) = tokio::io::split(server);
    let (client_in, mut client_out) = tokio::io::split(client);
    let mut lines = BufReader::new(client_in).lines();
    let home = root.join(".feather");
    let server = feather::rpc::run(
        agent(session, tools.clone(), FauxScript::new().with_text("x")),
        BufReader::new(server_in),
        server_out,
        &home,
        &root,
        move |s| Ok(agent(s, tools.clone(), FauxScript::new().with_text("x"))),
    );

    let client = async {
        client_out
            .write_all(
                b"{\"id\":\"sn\",\"type\":\"set_session_name\",\"name\":\"my-feature\"}\n{\"id\":\"st\",\"type\":\"get_state\"}\n",
            )
            .await
            .unwrap();
        let sn = record(&mut lines).await;
        assert_eq!(sn["id"], "sn");
        assert_eq!(sn["success"], true);
        let st = record(&mut lines).await;
        assert_eq!(st["id"], "st");
        assert_eq!(st["data"]["sessionName"], "my-feature");

        client_out
            .write_all(b"{\"id\":\"ge\",\"type\":\"get_entries\"}\n{\"id\":\"gf\",\"type\":\"get_fork_messages\"}\n")
            .await
            .unwrap();
        let ge = record(&mut lines).await;
        assert_eq!(ge["id"], "ge");
        assert_eq!(ge["success"], true);
        let entries = ge["data"]["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 2);
        assert!(entries[0]["parentId"].is_null());
        assert_eq!(entries[1]["parentId"], entries[0]["id"]);
        assert_eq!(ge["data"]["leafId"], entries[1]["id"]);

        let gf = record(&mut lines).await;
        assert_eq!(gf["id"], "gf");
        assert_eq!(gf["success"], true);
        let messages = gf["data"]["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["text"], "first prompt");
        client_out.shutdown().await.unwrap();
    };

    let (_, result) = tokio::join!(client, server);
    result.unwrap();
    std::fs::remove_dir_all(root).unwrap();
}
