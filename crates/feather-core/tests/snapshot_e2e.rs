//! Snapshot + rewind semantics, exercised through `Session` (the same path
//! `/rewind` takes in the TUI).
//!
//! directory files are restored too: every turn is a git checkpoint
//! (pi-task-tracker style, see gitckpt.rs). A repo created by feather gets an
//! exact tree restore on rewind; a pre-existing repo gets content restore only.

use feather_core::message::Message;
use feather_core::session::Session;

fn temp(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!(
        "feather-snap-e2e-{tag}-{}-{}",
        std::process::id(),
        feather_core::message::now_millis()
    ));
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn rewind_restores_context_and_keeps_transcript_and_files() {
    let home = temp("home");
    let cwd = temp("cwd");

    // a file the "agent" created during turn 2
    let workfile = cwd.join("generated.txt");
    std::fs::write(&workfile, "created in turn 2").unwrap();

    let mut s = Session::new(&home, &cwd).unwrap();
    assert_eq!(s.turn(), 0);

    // turn 1: user asks; assistant writes a.txt via the write tool — the tool
    // call is what makes the turn's git checkpoint include the file
    let _ = s.push(Message::user_text("make a file"));
    let _ = std::fs::write(cwd.join("a.txt"), "v1");
    let _ = s.push(Message::assistant(
        vec![feather_core::message::ContentBlock::ToolCall(
            feather_core::message::ToolCall {
                id: "c1".into(),
                name: "write".into(),
                arguments: serde_json::json!({"path": "a.txt"}),
            },
        )],
        "test-model",
        feather_core::message::Api::Faux,
        feather_core::message::Usage::default(),
        feather_core::message::StopReason::ToolUse,
    ));
    s.commit_turn().unwrap();
    // turn 2: follow-up
    // turn 2: follow-up writes generated.txt
    let _ = s.push(Message::user_text("now create generated.txt"));
    let _ = std::fs::write(&workfile, "created in turn 2");
    let _ = s.push(Message::assistant(
        vec![feather_core::message::ContentBlock::ToolCall(
            feather_core::message::ToolCall {
                id: "c2".into(),
                name: "write".into(),
                arguments: serde_json::json!({"path": "generated.txt"}),
            },
        )],
        "test-model",
        feather_core::message::Api::Faux,
        feather_core::message::Usage::default(),
        feather_core::message::StopReason::ToolUse,
    ));
    let turn2 = s.commit_turn().unwrap();
    assert_eq!(turn2, 2);
    assert_eq!(s.messages().len(), 4);

    // rewind to turn 1
    let discarded = s.rewind_to(1).unwrap();
    assert_eq!(
        discarded.len(),
        4,
        "the replaced (old) message list is returned"
    );
    assert_eq!(s.turn(), 1);
    assert_eq!(s.messages().len(), 2);
    assert_eq!(s.messages()[0].role, feather_core::message::Role::User);
    assert_eq!(s.messages()[1].role, feather_core::message::Role::Assistant);

    // the raw redb audit keeps the full history (rewind is additive)
    let raw = s.raw_messages().unwrap();
    assert_eq!(raw.len(), 4, "every pushed message");
    assert!(raw[2].text() == "now create generated.txt");

    // snapshots survive the rewind: forward-rewind is possible
    assert_eq!(s.latest_snapshot_turn().unwrap(), 2);

    // working directory IS restored (the whole point of git checkpoints):
    // feather created the repo (no .git above the temp cwd) → exact tree
    // restore — the turn-2 file is gone, the turn-1 file is back
    assert!(
        !workfile.exists(),
        "later turn's file removed by managed rewind"
    );
    assert_eq!(std::fs::read_to_string(cwd.join("a.txt")).unwrap(), "v1");

    // …and the session keeps working after a rewind: commit turn 3
    let _ = s.push(Message::user_text("continue"));
    s.commit_turn().unwrap();
    assert_eq!(s.turn(), 2);

    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&cwd);
}

#[test]
fn rewind_to_unknown_turn_is_an_error_not_a_corruption() {
    let home = temp("home2");
    let cwd = temp("cwd2");
    let mut s = Session::new(&home, &cwd).unwrap();
    let _ = s.push(Message::user_text("hi"));
    s.commit_turn().unwrap();
    assert!(s.rewind_to(99).is_err());
    // state untouched
    assert_eq!(s.turn(), 1);
    assert_eq!(s.messages().len(), 1);
    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&cwd);
}

#[test]
fn reopen_after_rewind_without_a_new_prompt_restores_current() {
    let home = temp("home-rewind");
    let cwd = temp("cwd-rewind");
    let mut s = Session::new(&home, &cwd).unwrap();
    s.push(Message::user_text("one")).unwrap();
    s.commit_turn().unwrap();
    s.push(Message::user_text("two")).unwrap();
    s.commit_turn().unwrap();
    s.push(Message::user_text("three")).unwrap();
    s.commit_turn().unwrap();
    s.rewind_to(1).unwrap();
    let dir = s.dir().to_path_buf();
    drop(s);

    // durable current pointer: no further prompt was accepted, so resume must
    // restore turn 1, not the newest snapshot (turn 3)
    let reopened = Session::open(&dir, &home).unwrap();
    assert_eq!(reopened.turn(), 1);
    assert_eq!(reopened.messages().len(), 1);
    assert_eq!(reopened.messages()[0].text(), "one");
    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&cwd);
}
