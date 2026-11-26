//! How much of a session's snapshot store is dead weight?
//!
//! `SnapshotStore::vacuum` exists to drop chunks no snapshot references, but
//! nothing in the production code calls it — only a test does. This measures what
//! that costs: how many chunks are unreachable, and how many bytes they hold.
//!
//! Read-only on the input side: it reports, and only rewrites when passed
//! `--vacuum`, so a run cannot destroy history by accident.

use feather_core::snapshot::SnapshotStore;
use std::path::PathBuf;

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1).collect::<Vec<_>>();
    let apply = args.iter().any(|a| a == "--vacuum");
    args.retain(|a| a != "--vacuum");

    let mut dirs: Vec<PathBuf> = if args.is_empty() {
        // Every session under the default home, newest first is not needed here.
        let home = feather::config::feather_home();
        std::fs::read_dir(home.join("sessions"))?
            .filter_map(|e| e.ok())
            .map(|e| e.path().join("snapshots"))
            .filter(|p| p.join("snapshots.redb").exists())
            .collect()
    } else {
        args.into_iter().map(PathBuf::from).collect()
    };
    dirs.sort();

    let mut total_before = 0u64;
    let mut total_after = 0u64;
    let mut total_dead_chunks = 0usize;

    for dir in &dirs {
        let file = dir.join("snapshots.redb");
        let before = std::fs::metadata(&file).map(|m| m.len()).unwrap_or(0);
        let store = match SnapshotStore::open(dir) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("{dir:?}: cannot open: {e}");
                continue;
            }
        };
        let (chunks, referenced) = match store.chunk_census() {
            Ok(c) => c,
            Err(e) => {
                eprintln!("{dir:?}: cannot census: {e}");
                continue;
            }
        };
        let dead = chunks.saturating_sub(referenced);
        let removed = if apply {
            store.vacuum().unwrap_or(0)
        } else {
            0
        };
        // Reopen so the reported size is post-vacuum, and drop the handle first so
        // the file is not held open across the measurement.
        drop(store);
        let after = std::fs::metadata(&file).map(|m| m.len()).unwrap_or(0);

        total_before += before;
        total_after += after;
        total_dead_chunks += dead;
        println!(
            "{:>8} KiB -> {:>8} KiB  chunks={chunks} referenced={referenced} dead={dead}{}  {}",
            before / 1024,
            after / 1024,
            if apply {
                format!(" removed={removed}")
            } else {
                String::new()
            },
            dir.parent()
                .and_then(|p| p.file_name())
                .and_then(|n| n.to_str())
                .unwrap_or("?")
        );
    }

    println!(
        "\ntotal {:.1} MiB -> {:.1} MiB ({} dead chunks across {} stores){}",
        total_before as f64 / (1024.0 * 1024.0),
        total_after as f64 / (1024.0 * 1024.0),
        total_dead_chunks,
        dirs.len(),
        if apply {
            ""
        } else {
            " — dry run, pass --vacuum to reclaim"
        }
    );
    Ok(())
}
