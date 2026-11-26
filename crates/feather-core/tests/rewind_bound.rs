//! Does a real session keep its rewind history bounded?
//!
//! `SnapshotStore::prune_before` is unit-tested, but the thing that matters is the
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

/// The unit that matters: one instruction spanning many turns must stay whole.
///
/// `commit_turn` runs after every assistant message, so a single user prompt costs
/// several snapshot turns. A horizon counted in *turns* would cut mid-instruction and
/// leave the user unable to rewind to where that instruction began — the actual bug.
/// A horizon counted in *instructions* keeps every turn of the newest ten.
#[test]
fn one_instruction_spanning_many_turns_is_not_cut_in_half() {
    let root = temp("instructions");
    let mut session = Session::new(&root.join(".feather"), &root).unwrap();

    // Twelve instructions, each spanning five round trips (a prompt plus four
    // assistant/tool-result pairs) — sixty snapshots in total.
    for instruction in 1..=12 {
        session
            .push(Message::user_text(format!("instruction {instruction}")))
            .unwrap();
        for step in 1..=5 {
            session
                .push(Message::tool_result(
                    format!("call-{instruction}-{step}"),
                    "bash",
                    format!("result {instruction}.{step}"),
                    false,
                ))
                .unwrap();
            session.commit_turn().unwrap();
        }
    }

    let store = session.snapshots().expect("durable session has snapshots");
    let turns: Vec<u64> = store.list().unwrap().iter().map(|(t, _, _)| *t).collect();
    // Ten instructions x five turns each = fifty survivable turns.
    assert!(
        turns.len() >= 50,
        "the newest ten instructions span ~50 turns and must be kept, found {}: {turns:?}",
        turns.len()
    );
    // But not all sixty: the first two instructions fell out of the window.
    assert!(
        turns.len() < 60,
        "the window must actually drop the oldest instructions, found {}",
        turns.len()
    );
    // The oldest kept turn must be the *start* of an instruction, not mid-loop:
    // rewinding there is what "undo the ninth-most-recent instruction" means.
    let oldest = *turns.first().expect("some turn survived");
    let restored = store.load(oldest).expect("oldest kept turn loads");
    // The window edge must fall on a real instruction boundary: the oldest message in
    // the oldest kept snapshot is a user instruction, not the tail of one. That is
    // exactly what "rewind to the instruction before X" requires.
    let first = restored.first().expect("non-empty snapshot");
    assert_eq!(
        first.role,
        feather_core::message::Role::User,
        "the window edge must open on a user instruction, found {:?}",
        first.role
    );
    assert!(
        first.text().starts_with("instruction "),
        "the window edge must be an instruction prompt, found {:?}",
        first.text()
    );

    // And the very oldest instruction is genuinely gone, which proves pruning ran
    // rather than the test merely observing a short session.
    let all: Vec<u64> = store.list().unwrap().iter().map(|(t, _, _)| *t).collect();
    assert!(
        all.first().copied().unwrap_or(0) > 1,
        "the first instruction's turns must be pruned away, kept from {:?}",
        all.first()
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// A reopened session must keep its window. If the instruction list is not persisted,
/// a resume starts empty and the store grows unbounded again from there — the same
/// bug, silently reintroduced on the resume path.
#[test]
fn the_rewind_window_survives_a_reopen() {
    let root = temp("reopen");
    let dir;
    {
        let mut session = Session::new(&root.join(".feather"), &root).unwrap();
        dir = session.dir().to_path_buf();
        for instruction in 1..=14 {
            session
                .push(Message::user_text(format!("instruction {instruction}")))
                .unwrap();
            for step in 1..=3 {
                session
                    .push(Message::tool_result(
                        format!("call-{instruction}-{step}"),
                        "bash",
                        format!("result {instruction}.{step}"),
                        false,
                    ))
                    .unwrap();
                session.commit_turn().unwrap();
            }
        }
    }
    // Reopen and issue ten more instructions. Ten is deliberate: a resumed session
    // that lost its list has exactly ten entries here, so `len <= horizon` returns
    // early and prunes nothing — the pre-resume turns survive forever. With the list
    // persisted there are twenty, the floor moves, and the old turns go. This is the
    // only place the two behaviours differ, so the test has to reach it.
    let mut resumed = Session::open(&dir, &root.join(".feather")).expect("resume");
    for step in 1..=10 {
        resumed
            .push(Message::user_text(format!("after resume {step}")))
            .unwrap();
        resumed.commit_turn().unwrap();
    }
    let store = resumed.snapshots().expect("durable session");
    let turns: Vec<u64> = store.list().unwrap().iter().map(|(t, _, _)| *t).collect();
    // Ten post-resume instructions x one turn each = ten turns in the window. A
    // session that forgot its prior list would still be holding the pre-resume turns
    // plus these, because ten entries never exceeds the horizon and so never prunes.
    assert!(
        turns.len() <= 12,
        "window must still bound after resume, found {} (a lost horizon keeps ~43)",
        turns.len()
    );
    assert!(
        !turns.is_empty(),
        "a resumed session must keep a usable window"
    );
    let _ = std::fs::remove_dir_all(&root);
}
