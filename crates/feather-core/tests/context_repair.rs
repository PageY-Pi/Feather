//! A context that providers reject must be repaired, never resumed as-is.
//!
//! The failure this guards: compaction once installed a kept span that opened
//! on a tool result (the `tool_calls` assistant it answered stayed behind in
//! the summarized prefix). Every later request then 400s —
//! "Messages with role 'tool' must be a response to a preceding message with
//! 'tool_calls'" — and switching models does not help, because the broken list
//! is durable session state, not a provider quirk.

use feather_core::message::{ContentBlock, Message, Role, StopReason, ToolCall, Usage};
use feather_core::session::Session;

fn temp(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!(
        "feather-repair-{tag}-{}-{}",
        std::process::id(),
        feather_core::message::now_millis()
    ));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn assistant_with_call(id: &str, name: &str) -> Message {
    Message::assistant(
        vec![ContentBlock::ToolCall(ToolCall {
            id: id.into(),
            name: name.into(),
            arguments: serde_json::json!({"command": "ls"}),
        })],
        "test-model",
        feather_core::message::Api::Faux,
        Usage::default(),
        StopReason::ToolUse,
    )
}

fn assistant_text(text: &str) -> Message {
    Message::assistant(
        vec![ContentBlock::Text { text: text.into() }],
        "test-model",
        feather_core::message::Api::Faux,
        Usage::default(),
        StopReason::Stop,
    )
}

/// The exact durable shape the bad cut produced: summary, then a tool result
/// whose call is gone, then the rest of the kept span.
#[test]
fn resume_repairs_a_snapshot_that_opens_on_an_orphan_tool_result() {
    let home = temp("home");
    let cwd = temp("cwd");
    let mut s = Session::new(&home, &cwd).unwrap();
    let dir = s.dir().to_path_buf();
    s.push(Message::user_text("go")).unwrap();
    s.push(assistant_with_call("call_00_gone", "bash")).unwrap();
    s.push(Message::tool_result("call_00_gone", "bash", "ran", false))
        .unwrap();
    let turn = s.commit_turn().unwrap();

    let broken = vec![
        feather_core::compaction::summary_message("earlier work", &[], &[]),
        Message::tool_result("call_00_gone", "bash", "395: chosen_preset = ...", false),
        assistant_text("carrying on"),
    ];
    {
        let snapshots = s.snapshots().expect("durable session");
        snapshots.snapshot(turn, &broken).unwrap();
        snapshots.set_current(turn).unwrap();
    }
    drop(s);

    let mut reopened = Session::open(&dir, &home).unwrap();
    assert!(
        !reopened
            .messages()
            .iter()
            .any(|m| m.role == Role::ToolResult),
        "the orphan result must not survive a resume: {:?}",
        reopened.messages()
    );
    assert_eq!(reopened.messages().len(), 2);
    assert_eq!(reopened.messages()[1].text(), "carrying on");
    // not silent: the repair is reported as a user-facing note, never as a
    // model-facing runtime diagnostic (that channel is context input)
    let notes = reopened.take_notes();
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert!(notes[0].contains("context repaired (resume)"), "{notes:?}");
    assert!(reopened.take_runtime_diagnostics().is_empty());

    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&cwd);
}

/// Rewind keeps its contract — it restores the snapshot verbatim, even a
/// broken one; the repair happens before the next request instead.
#[test]
fn rewind_restores_verbatim_and_repair_is_idempotent() {
    let home = temp("home2");
    let cwd = temp("cwd2");
    let mut s = Session::new(&home, &cwd).unwrap();
    s.push(Message::user_text("one")).unwrap();
    s.commit_turn().unwrap();
    assert_eq!(s.messages().len(), 1);
    // a healthy context is never rewritten by the pre-request repair
    s.repair_context("prompt");
    assert_eq!(s.messages().len(), 1);
    assert!(s.take_notes().is_empty());
    s.repair_context("prompt");
    assert!(s.take_notes().is_empty());
    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&cwd);
}

/// The request body is what the provider actually rejects, so assert on the
/// conversion: no `tool` message may appear without a `tool_calls` assistant
/// directly before it. Before the repair this body is exactly what produced
/// the 400 in the field.
#[test]
fn repaired_context_converts_to_a_body_providers_accept() {
    let broken = vec![
        feather_core::compaction::summary_message("earlier work", &[], &[]),
        Message::tool_result("call_00_gone", "bash", "395: chosen_preset = ...", false),
        assistant_text("carrying on"),
    ];
    let body = feather_core::provider::openai::OpenAiProvider::convert_messages(&broken);
    assert_eq!(body[1]["role"], "tool");
    assert!(
        body[0].get("tool_calls").is_none(),
        "the bug: a `tool` message answered by nothing"
    );

    let mut repaired = broken.clone();
    assert_eq!(
        feather_core::compaction::repair_tool_pairs(&mut repaired),
        1
    );
    let body = feather_core::provider::openai::OpenAiProvider::convert_messages(&repaired);
    let mut answered = true;
    for message in &body {
        if message["role"] == "tool" {
            assert!(
                answered,
                "tool message with no preceding tool_calls: {message}"
            );
            answered = false;
        } else {
            answered = message.get("tool_calls").is_some();
        }
    }
}
