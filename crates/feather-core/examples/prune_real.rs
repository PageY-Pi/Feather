//! Measure `prune_to_last` on a copy of a real session's snapshot database.
//!
//! The synthetic probe in `prune_probe.rs` could not answer the real question,
//! because its `current_turn` pointer was never set and every snapshot held the
//! whole history, so nothing was ever unreferenced. This runs against a real
//! store copied out of `~/.feather/sessions`, which is the only shape that says
//! whether pruning buys disk.
//!
//! Usage: `cargo run --release -p feather-core --example prune_real -- <dir> [keep]`

use feather_core::snapshot::SnapshotStore;

fn mib(bytes: u64) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let dir = args
        .first()
        .map(std::path::PathBuf::from)
        .ok_or("usage: prune_real <snapshot dir> [keep]")?;
    let keep: u64 = args.get(1).map(|k| k.parse().unwrap_or(10)).unwrap_or(10);
    let path = dir.join("snapshots.redb");

    let store = SnapshotStore::open(&dir)?;
    let before = std::fs::metadata(&path)?.len();
    let turns = store.list()?;
    let (stored, referenced) = store.chunk_census()?;
    println!(
        "before: {:.1} MiB, {} turns, {stored} chunks ({referenced} referenced), live turn {}",
        mib(before),
        turns.len(),
        store.current_turn()?
    );
    // The newest snapshot's message count bounds what a full context costs: if the
    // newest turn holds every message, pruning older turns cannot free those
    // chunks no matter how many turns are dropped.
    if let Some((turn, count, _)) = turns.last() {
        println!("newest turn {turn} references {count} messages");
    }

    let dropped = store.prune_to_last(keep)?;
    let after_prune = std::fs::metadata(&path)?.len();
    let (stored2, referenced2) = store.chunk_census()?;
    println!(
        "after prune(keep={keep}): dropped {dropped} turns, {:.1} MiB, \
         {stored2} chunks ({referenced2} referenced), {} turns left",
        mib(after_prune),
        store.list()?.len()
    );

    // redb returns pages to the OS only through compaction, and only with a
    // unique handle. Drop ours so compaction is even possible, then reopen with
    // exclusive access.
    drop(store);
    let mut db = redb::Database::builder().open(&path)?;
    match db.compact() {
        Ok(did) => println!("compact(): performed={did}"),
        Err(e) => println!("compact() refused: {e}"),
    }
    drop(db);
    let after_compact = std::fs::metadata(&path)?.len();
    println!(
        "after compact: {:.1} MiB (total {:.0}% of before)",
        mib(after_compact),
        100.0 * after_compact as f64 / before as f64
    );

    let reopened = SnapshotStore::open(&dir)?;
    let turns_after = reopened.list()?;
    println!("turns still loadable: {}", turns_after.len());
    // Prove the surviving history is intact: the newest turn must still load.
    if let Some((turn, _, _)) = turns_after.last() {
        match reopened.load(*turn) {
            Ok(msgs) => println!("newest turn {turn} loads: {} messages", msgs.len()),
            Err(e) => println!("newest turn {turn} FAILED to load: {e}"),
        }
    }
    Ok(())
}
