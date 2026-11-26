//! What does a ten-*instruction* rewind window cost on a copy of a real session?
//!
//! `prune_real` measures the old unit (ten snapshot turns) and its premise — that a
//! context holds every prior instruction — is wrong for any compacted session: the
//! real 2372-turn profile held at most **four** instructions in any single snapshot,
//! so counting instructions per context finds no boundary at all. This drives the
//! actual `Session`/`SnapshotStore` path instead, computing the floor the way
//! `Session::rewind_floor` does.
//!
//! Usage: `cargo run --release -p feather-core --example prune_instructions -- <copied session dir> [keep]`

use feather_core::message::{Message, Role};
use feather_core::session_store::SessionStore;
use feather_core::snapshot::SnapshotStore;

fn mib(bytes: u64) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let dir = args
        .first()
        .map(std::path::PathBuf::from)
        .ok_or("usage: prune_instructions <copied session dir> [keep]")?;
    let keep: usize = args.get(1).map(|k| k.parse().unwrap_or(10)).unwrap_or(10);
    let snap_path = dir.join("snapshots").join("snapshots.redb");
    let before = std::fs::metadata(&snap_path)?.len();

    let store = SnapshotStore::open(&dir.join("snapshots"))?;
    println!(
        "before: {:.1} MiB, {} snapshots, live turn {}",
        mib(before),
        store.list()?.len(),
        store.current_turn()?
    );

    // The authoritative boundary list: what the session persisted. A session written
    // before this feature has none, which is itself worth reporting — it means the
    // window has to be rebuilt once from the audit trail.
    let boundaries = store.instruction_turns()?;
    if !boundaries.is_empty() {
        println!(
            "{} persisted instruction boundaries; window keeps the newest {keep}",
            boundaries.len()
        );
        let floor = if boundaries.len() <= keep {
            0
        } else {
            boundaries[boundaries.len() - keep]
        };
        println!("floor turn {floor} (from persisted boundaries)");
        if floor == 0 {
            println!("nothing to prune at this horizon");
            return Ok(());
        }
        return prune(store, &snap_path, floor, before);
    }

    // No persisted boundaries (an older session). Rebuild them from the audit trail in
    // the order the turns were committed: the audit rows are the whole history, so the
    // i-th instruction's turn is the position it occupies in that history. Snapshot
    // turns and audit rows are both append-ordered, so matching the i-th instruction to
    // the i-th snapshot underestimates nothing except compression side-effects.
    let session = SessionStore::open(&dir)?;
    let messages: Vec<Message> = session.raw_messages()?;
    let instructions: Vec<usize> = messages
        .iter()
        .enumerate()
        .filter(|(_, m)| {
            m.role == Role::User
                && m.tool_call_id.is_none()
                && !m.text().starts_with("## Conversation summary")
                && !m.text().starts_with("<feather-runtime-diagnostic>")
        })
        .map(|(i, _)| i)
        .collect();
    println!(
        "no persisted boundaries; audit trail has {} instructions in {} messages",
        instructions.len(),
        messages.len()
    );

    // Approximate the turn a given audit row landed on by spreading the snapshots over
    // the messages proportionally. That is exact only for an uncompacted session, so it
    // is reported as the estimate it is.
    let turns = store.list()?;
    let last_turn = turns.last().map(|(t, _, _)| *t).unwrap_or(0);
    let target_index = instructions.len().saturating_sub(keep);
    if target_index == 0 {
        println!("nothing to prune at this horizon");
        return Ok(());
    }
    let fraction = instructions[target_index] as f64 / messages.len() as f64;
    let floor = ((fraction * last_turn as f64) as u64).max(1);
    println!(
        "estimated floor turn {floor} (row {} of {}; proportional, not exact)",
        instructions[target_index],
        messages.len()
    );
    drop(session);
    prune(store, &snap_path, floor, before)
}

fn prune(
    store: SnapshotStore,
    snap_path: &std::path::Path,
    floor: u64,
    before: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    let dropped = store.prune_before(floor)?;
    println!(
        "after prune: dropped {dropped} snapshots, {:.1} MiB, {} snapshots left",
        mib(std::fs::metadata(snap_path)?.len()),
        store.list()?.len()
    );
    let path = snap_path.to_path_buf();
    // Compaction needs a *unique* handle: ours and the process-wide cache both hold
    // one. Drop the store and let `compact_file` clear the cache entry, exactly as
    // `Session::prune_rewind_history` does. Skipping this fails with
    // "Database already open. Cannot acquire lock."
    drop(store);
    let did = SnapshotStore::compact_file(&path)?;
    println!("compact(): performed={did}");
    let after = std::fs::metadata(&path)?.len();
    println!(
        "after compact: {:.1} MiB ({:.0}% of before)",
        mib(after),
        100.0 * after as f64 / before as f64
    );
    Ok(())
}
