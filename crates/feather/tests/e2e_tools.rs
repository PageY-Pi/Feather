//! End-to-end offline validation: the full agent kernel loop with the REAL
//! tool set, driven by a scripted (faux) model through the same Anthropic SSE
//! parsing path a live provider uses. Asserts the output formats match pi:
//! `anchor│content` read rows, post-edit diffs with fresh anchors, hashline
//! error codes, `[exit 0]` bash results, task ids, and anchor_grep sections.
//!
//! Phase A drives the loop through the agent (anchor-independent tools);
//! Phase B drives the anchor tools directly through the same registry,
//! chaining anchors extracted from earlier results — exactly how a model
//! consumes them in pi.

use feather::bash::BashTool;
use feather::tasks::{TaskCreateTool, TaskListTool, TaskStore};
use feather_core::agent::{Agent, AgentConfig, AgentEvent, ProviderRef};
use feather_core::message::{Api, StopReason, ToolCall};
use feather_core::provider::faux::{FauxProvider, FauxScript};
use feather_core::session::Session;
use feather_core::tool::ToolRegistry;
use feather_hashline::store::AnchorStore;
use feather_hashline::tools::{
    AnchorGrepTool, InsertTool, ReadTool, ReplaceTool, ReplaceWithinTool, UndoTool, WriteTool,
};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

fn temp_root(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "feather-e2e-{tag}-{}-{}",
        std::process::id(),
        feather_core::message::now_millis()
    ));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn registry(root: &Path, anchor_home: &Path) -> ToolRegistry {
    let store = Arc::new(AnchorStore::new(anchor_home));
    let mut reg = ToolRegistry::new();
    reg.add(Arc::new(ReadTool {
        store: store.clone(),
    }))
    .add(Arc::new(WriteTool {
        store: store.clone(),
    }))
    .add(Arc::new(ReplaceTool {
        store: store.clone(),
    }))
    .add(Arc::new(ReplaceWithinTool {
        store: store.clone(),
    }))
    .add(Arc::new(InsertTool {
        store: store.clone(),
    }))
    .add(Arc::new(UndoTool { store }))
    .add(Arc::new(AnchorGrepTool {
        root: root.to_path_buf(),
        store: Arc::new(AnchorStore::new(anchor_home)),
    }))
    .add(Arc::new(BashTool {
        timeout: Duration::from_secs(30),
        cwd: root.to_path_buf(),
    }));
    reg
}

async fn call(
    reg: &ToolRegistry,
    name: &str,
    args: serde_json::Value,
) -> feather_core::tool::ToolOutcome {
    let (_tx, rx) = tokio::sync::watch::channel(false);
    reg.execute_call(
        &ToolCall {
            id: "t1".into(),
            name: name.into(),
            arguments: args,
        },
        rx,
    )
    .await
}

/// Extract the anchor of the row whose content is exactly `needle`.
fn anchor_of(result: &str, needle: &str) -> String {
    for line in result.lines() {
        let body = line.strip_prefix('+').unwrap_or(line);
        if let Some((anchor, text)) = body.split_once('│') {
            if text.trim_end() == needle {
                return anchor.to_string();
            }
        }
    }
    panic!("no anchor found for {needle:?} in:\n{result}");
}

#[tokio::test]
async fn agent_loop_end_to_end_matches_pi_formats() {
    let root = temp_root("loop");
    let anchor_home = root.join("anchors");
    std::fs::create_dir_all(&anchor_home).unwrap();

    let file = root.join("demo.txt");
    let script = FauxScript::new()
        .with_tool(
            "write",
            serde_json::json!({"path": file.display().to_string(),
                               "content": "alpha line\nbeta line\ngamma line\n"}),
        )
        .with_tool("bash", serde_json::json!({"command": "echo hello-e2e"}))
        .with_tool(
            "task_create",
            serde_json::json!({"title": "write the demo", "description": "e2e check"}),
        )
        .with_tool("task_list", serde_json::json!({}))
        .with_tool(
            "anchor_grep",
            serde_json::json!({"pattern": "beta", "path": root.display().to_string()}),
        )
        .with_text("all done");
    let provider = FauxProvider::new(script);

    let mut reg = registry(&root, &anchor_home);
    let tasks = Arc::new(TaskStore::open(&root));
    reg.add(Arc::new(TaskCreateTool(tasks.clone())))
        .add(Arc::new(TaskListTool(tasks.clone())));

    let session_dir = root.join("session");
    std::fs::create_dir_all(&session_dir).unwrap();
    let session = Session::new(&session_dir, &root).unwrap();
    let config = AgentConfig {
        thinking_level: None,
        model: "faux-1".into(),
        api: Api::AnthropicMessages,
        system_prompt: "e2e".into(),
        max_tokens: 1024,
        temperature: None,
        context_window: 100_000,
        compaction: Default::default(),
        compaction_model: None,
        tools: reg.defs().into(),
    };
    let mut agent = Agent::new(
        config,
        session,
        Arc::new(reg),
        ProviderRef::Faux(provider),
        None,
    );
    let mut events = agent.subscribe();

    let res = agent.prompt("run the demo").await.expect("run completes");
    assert_eq!(res.final_text, "all done");
    assert_eq!(res.stop_reason, StopReason::Stop);
    assert!(
        res.turns >= 5,
        "one turn per scripted response: {}",
        res.turns
    );
    assert!(res.usage.output > 0, "usage accumulated");

    // ---- collect tool results from the event stream
    let mut tool_results: Vec<(String, String, bool)> = Vec::new();
    let mut saw_text_delta = false;
    let mut saw_run_end = false;
    while let Ok(ev) = events.try_recv() {
        match ev {
            AgentEvent::ToolEnd {
                call,
                text,
                is_error,
            } => tool_results.push((call.name, text, is_error)),
            AgentEvent::TextDelta(_) => saw_text_delta = true,
            AgentEvent::RunEnd(_) => saw_run_end = true,
            _ => {}
        }
    }
    assert!(saw_text_delta, "final text streamed");
    assert!(saw_run_end, "run end observed");
    assert_eq!(tool_results.len(), 5, "one ToolEnd per scripted call");

    // ---- write: pi-style post-edit diff with fresh anchors
    let (name, text, is_err) = &tool_results[0];
    assert_eq!(name, "write");
    assert!(!is_err, "{text}");
    assert!(
        text.lines().any(|l| {
            let body = l.strip_prefix('+').unwrap_or(l);
            body.split_once('│')
                .map(|(a, t)| a.len() == 4 && t == "alpha line")
                .unwrap_or(false)
        }),
        "write returns +anchor│ rows:\n{text}"
    );

    // ---- bash: stdout + pi-style exit marker
    let (name, text, is_err) = &tool_results[1];
    assert_eq!(name, "bash");
    assert!(!is_err, "{text}");
    assert!(text.contains("hello-e2e"), "{text}");
    assert!(text.contains("[exit 0]"), "{text}");

    // ---- tasks: pi-task-tracker ids and status rendering
    let (name, text, is_err) = &tool_results[2];
    assert_eq!(name, "task_create");
    assert!(!is_err, "{text}");
    assert!(
        text.contains("created t001 [pending] write the demo"),
        "{text}"
    );
    let (name, text, is_err) = &tool_results[3];
    assert_eq!(name, "task_list");
    assert!(!is_err, "{text}");
    assert!(text.contains("t001 [pending] write the demo"), "{text}");

    // ---- anchor_grep: section header + `lineNo │ anchor│content` rows
    let (name, text, is_err) = &tool_results[4];
    assert_eq!(name, "anchor_grep");
    assert!(!is_err, "{text}");
    let header = format!("=== {} ===", file.display());
    assert!(text.contains(&header), "section header:\n{text}");
    assert!(
        text.lines().any(|l| {
            // `12 │ AbCd│beta line`
            let mut parts = l.splitn(2, " │ ");
            match (parts.next(), parts.next()) {
                (Some(no), Some(rest)) => {
                    no.trim().parse::<u64>().is_ok()
                        && rest
                            .split_once('│')
                            .map(|(a, t)| a.len() == 4 && t == "beta line")
                            .unwrap_or(false)
                }
                _ => false,
            }
        }),
        "grep row format:\n{text}"
    );

    // ---- persistence: task survived to disk
    assert!(root.join("tasks.redb").exists(), "tasks persisted");

    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn anchor_flow_chains_like_pi() {
    let root = temp_root("anchors");
    let anchor_home = root.join("anchors");
    std::fs::create_dir_all(&anchor_home).unwrap();
    let reg = registry(&root, &anchor_home);
    let file = root.join("demo.txt");
    let p = file.display().to_string();

    // write seeds three lines; result carries fresh +anchor│ rows
    let out = call(
        &reg,
        "write",
        serde_json::json!({"path": p, "content": "alpha line\nbeta line\ngamma line\n"}),
    )
    .await;
    assert!(!out.is_error, "{}", out.text);
    let a_alpha = anchor_of(&out.text, "alpha line");
    let a_beta = anchor_of(&out.text, "beta line");
    let a_gamma = anchor_of(&out.text, "gamma line");

    // read: `anchor│content` rows, anchors stable across reads
    let out = call(&reg, "read", serde_json::json!({"path": p})).await;
    assert!(!out.is_error, "{}", out.text);
    assert_eq!(
        anchor_of(&out.text, "alpha line"),
        a_alpha,
        "anchors survive round-trips"
    );
    assert_eq!(anchor_of(&out.text, "beta line"), a_beta);

    // replace by anchor: `path:` header + pi-style diff (-old, +new);
    // the diff shows only the churn, untouched lines keep their anchors
    let out = call(
        &reg,
        "replace",
        serde_json::json!({"path": p, "remove_from": a_beta, "remove_to": a_beta,
                           "replacement_lines": "beta line edited"}),
    )
    .await;
    assert!(!out.is_error, "{}", out.text);
    // pi renders the bare diff: churn framed by Kept context rows
    assert!(
        out.text
            .lines()
            .any(|l| l.starts_with('-') && l.ends_with("beta line")),
        "removed row in diff:
{}",
        out.text
    );
    assert!(
        out.text.contains(" ... ") || out.text.lines().any(|l| l.starts_with(' ')),
        "context row or ellipsis marker present:
{}",
        out.text
    );
    let a_beta2 = anchor_of(&out.text, "beta line edited");
    assert_ne!(a_beta2, a_beta, "changed line gets a fresh anchor");
    let disk = std::fs::read_to_string(&file).unwrap();
    assert!(disk.contains("beta line edited"));

    // anchors survive edits: a fresh read still resolves the untouched alpha
    let out = call(&reg, "read", serde_json::json!({"path": p})).await;
    assert!(!out.is_error, "{}", out.text);
    assert_eq!(
        anchor_of(&out.text, "alpha line"),
        a_alpha,
        "anchors survive edits"
    );

    // replace_within: partial-line substitution by anchors
    let out = call(
        &reg,
        "replace_within",
        serde_json::json!({"path": p, "replace_from": a_beta2, "replace_to": a_beta2,
                           "replace_old": "edited", "replace_new": "v2"}),
    )
    .await;
    assert!(!out.is_error, "{}", out.text);
    assert!(std::fs::read_to_string(&file)
        .unwrap()
        .contains("beta line v2"));

    // insert after gamma
    let out = call(
        &reg,
        "insert",
        serde_json::json!({"path": p, "anchor": a_gamma, "direction": "after",
                           "lines": "delta line"}),
    )
    .await;
    assert!(!out.is_error, "{}", out.text);
    let a_delta = anchor_of(&out.text, "delta line");
    assert_eq!(a_delta.len(), 4);

    // mismatched old content must be rejected — pi's guarantee, not a lenient diff
    let out = call(
        &reg,
        "replace",
        serde_json::json!({"path": p, "remove_from": a_alpha, "remove_to": a_alpha,
                           "replacement_lines": "should not apply"}),
    )
    .await;
    // `replace` swaps the whole line range: this SUCCEEDS by design (it
    // replaces the alpha line). The mismatch case is `replace_within`:
    assert!(!out.is_error);
    let out = call(
        &reg,
        "replace_within",
        serde_json::json!({"path": p, "replace_from": a_alpha, "replace_to": a_alpha,
                           "replace_old": "no such text on this line", "replace_new": "x"}),
    )
    .await;
    assert!(
        out.is_error,
        "wrong replace_old must be an error: {}",
        out.text
    );
    assert!(
        out.text.contains("[E_"),
        "hashline error code surfaced: {}",
        out.text
    );

    // undo reverts the last change (the successful alpha replace)
    let disk_before = std::fs::read_to_string(&file).unwrap();
    let out = call(&reg, "undo_last_change", serde_json::json!({"path": p})).await;
    assert!(!out.is_error, "{}", out.text);
    let disk_after = std::fs::read_to_string(&file).unwrap();
    assert_ne!(
        disk_before, disk_after,
        "undo restored the previous content"
    );
    assert!(disk_after.contains("alpha line"));

    let _ = std::fs::remove_dir_all(&root);
}
