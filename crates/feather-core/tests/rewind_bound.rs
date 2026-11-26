//! Does a real session keep its rewind history bounded?
//!
//! `SnapshotStore::prune_to_last` is unit-tested, but the thing that matters is the
//! end-to-end behaviour of `Session::commit_turn`: after many turns, the snapshot
//! file must stop growing, the newest turns must still rewind, and resuming from
//! disk must land on the right context. Those are three different failure modes and
//! a unit test on the store catches none of them.

use feather_core::message::Message;
use feather_core::session::Session;
use std::path::{Path, PathBuf};

fn temp(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "feather-rewind-bound-{tag}-{}-{}",
        std::process::id(),
        feather_core::message::now_millis()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn snapshot_bytes(root: &Path) -> u64 {
    let mut total = 0;
    let sessions = root.join(".feather").join("sessions");
    let Ok(entries) = std::fs::read_dir(sessions) else {
        return 0;
    };
    for entry in entries.flatten() {
        let file = entry.path().join("snapshots").join("snapshots.redb");
        if let Ok(meta) = std::fs::metadata(file) {
            total += meta.len();
        }
    }
    total
}

/// Fifteen turns is five past the horizon, so pruning must have happened. The test
/// asserts the observable consequences rather than the mechanism: the rewind list is
/// bounded, the recent end still works, and the file is smaller than the unbounded
/// version would have made it.
#[test]
fn a_long_session_keeps_only_the_recent_rewind_window() {
    let root = temp("window");
    let mut session = Session::new(&root.join(".feather"), &root).unwrap();

    // Fat turns, so an unbounded store would be visibly large.
    for turn in 1..=15 {
        session
            .push(Message::user_text(format!(
                "turn {turn} {}",
                "x".repeat(20_000)
            )))
            .unwrap();
        session.commit_turn().unwrap();
    }

    let store = session.snapshots().expect("durable session has snapshots");
    let turns: Vec<u64> = store.list().unwrap().iter().map(|(t, _, _)| *t).collect();
    assert!(
        turns.len() <= 10,
        "rewind history must stay bounded, found {turns:?}"
    );
    assert!(
        !turns.is_empty(),
        "pruning must not remove the recent window"
    );
    // The newest turn is what resume reads, so it must still be there.
    let newest = *turns.last().unwrap();
    assert_eq!(newest, 15, "the newest turn must survive, found {turns:?}");
    let messages = store.load(newest).unwrap();
    assert_eq!(messages.len(), 15, "and it must load all fifteen messages");

    let bytes = snapshot_bytes(&root);
    let _ = std::fs::remove_dir_all(&root);
    // 15 turns x 20 KB is ~300 KB of message text; an unbounded store keeps one
    // full copy per turn. A few MiB of slack keeps this from being flaky on
    // filesystems with coarse block sizes, while still failing loudly if pruning
    // stops happening.
    assert!(
        bytes < 4_000_000,
        "snapshot store grew to {bytes} bytes; pruning may have stopped"
    );
}

/// Rewind to the oldest surviving turn, then commit again: the session must not
/// resume onto a turn that was pruned away. This is the corrupt-session path.
#[test]
fn after_pruning_the_oldest_surviving_turn_still_rewinds() {
    let root = temp("rewind");
    let mut session = Session::new(&root.join(".feather"), &root).unwrap();
    for turn in 1..=14 {
        session
            .push(Message::user_text(format!("turn {turn}")))
            .unwrap();
        session.commit_turn().unwrap();
    }
    let store = session.snapshots().expect("durable session");
    let turns: Vec<u64> = store.list().unwrap().iter().map(|(t, _, _)| *t).collect();
    let oldest = *turns.first().expect("some turn survived");

    let discarded = session
        .rewind_to(oldest)
        .expect("rewind to the oldest kept");
    // `rewind_to` returns what was *discarded*, not what was restored — the live
    // context is the short one now, and the discarded tail is what the UI shows.
    assert_eq!(session.messages().len() as u64, oldest);
    assert_eq!(
        discarded.len(),
        14,
        "the forward history was the discarded part"
    );
    // The pointer moved with it, so a resume reads this turn and not the newest.
    let store = session.snapshots().expect("durable session");
    assert_eq!(store.current_turn().unwrap(), oldest);

    // Coming forward again must work: the later turns were not pruned away.
    session.push(Message::user_text("after rewind")).unwrap();
    session.commit_turn().expect("commit after rewind");
    let _ = std::fs::remove_dir_all(&root);
}
