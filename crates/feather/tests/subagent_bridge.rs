//! Subagent abort-bridge and durable-record behaviour, driven through the
//! public `SubagentTool` surface (no private test hooks).

use feather::subagent::SubagentTool;
use feather_core::agent::ProviderRef;
use feather_core::provider::faux::{FauxProvider, FauxScript};
use feather_core::tool::AgentTool;
use std::path::{Path, PathBuf};
use std::sync::Arc;

fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "feather-sub-it-{tag}-{}-{}",
        std::process::id(),
        feather_core::message::now_millis()
    ));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn tool_for(root: &Path, session_dir: &Path) -> SubagentTool {
    let provider = Arc::new(ProviderRef::Faux(FauxProvider::new(
        FauxScript::new().with_text("done"),
    )));
    SubagentTool {
        provider,
        root: root.to_path_buf(),
        session_dir: session_dir.to_path_buf(),
        model: "faux-1".into(),
        max_tokens: 1024,
        context_window: 100_000,
        bash_timeout: std::time::Duration::from_secs(5),
        inherited_tools: None,
        use_policy: false,
    }
}

#[tokio::test]
async fn completed_run_persists_transcript_and_terminal_record() {
    let root = temp_dir("done-root");
    let session_dir = temp_dir("done-session");
    let tool = tool_for(&root, &session_dir);
    let (_tx, rx) = tokio::sync::watch::channel(false);
    let out = tool
        .execute(
            serde_json::json!({"prompt": "say hi", "description": "greet"}),
            rx,
        )
        .await;
    assert!(!out.is_error, "{}", out.text);
    assert!(out.text.ends_with("\ndone"));

    let store = feather_core::substore::SubagentStore::open(&session_dir).unwrap();
    let recs = store.list().unwrap();
    assert_eq!(recs.len(), 1);
    assert_eq!(recs[0].label, "greet");
    assert_eq!(recs[0].output.as_deref(), Some("done"));
    assert!(!store.transcript(&recs[0].id).unwrap().is_empty());
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&session_dir);
}

#[tokio::test]
async fn parent_already_aborted_is_not_reset_and_is_recorded() {
    let root = temp_dir("abort-root");
    let session_dir = temp_dir("abort-session");
    let tool = tool_for(&root, &session_dir);
    // the parent is already aborted before the subagent is even spawned; the
    // bridge must not let `Agent::prompt` reset it back to false
    let (_tx, rx) = tokio::sync::watch::channel(true);
    let out = tool
        .execute(serde_json::json!({"prompt": "do the thing"}), rx)
        .await;
    assert!(out.is_error, "aborted run must be an error: {}", out.text);
    assert!(out.text.contains("aborted"), "{}", out.text);

    let store = feather_core::substore::SubagentStore::open(&session_dir).unwrap();
    let recs = store.list().unwrap();
    assert_eq!(recs.len(), 1);
    assert!(
        recs[0].error.as_deref().unwrap_or("").contains("aborted"),
        "durable failure record: {:?}",
        recs[0]
    );
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&session_dir);
}

#[tokio::test]
async fn durable_store_open_failure_is_reported_not_swallowed() {
    let root = temp_dir("storefail-root");
    // the session path is a *file*, so the redb store cannot be created
    let not_a_dir = root.join("substore-file");
    std::fs::write(&not_a_dir, b"x").unwrap();
    let tool = tool_for(&root, &not_a_dir);
    let (_tx, rx) = tokio::sync::watch::channel(false);
    let out = tool.execute(serde_json::json!({"prompt": "x"}), rx).await;
    assert!(out.is_error, "{}", out.text);
    assert!(out.text.contains("store failed"), "{}", out.text);
    let _ = std::fs::remove_dir_all(&root);
}
