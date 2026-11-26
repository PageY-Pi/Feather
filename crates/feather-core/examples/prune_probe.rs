//! Does deleting rows actually give the bytes back?
//!
//! `SnapshotStore` prunes old turns by removing rows, but redb reclaims freed
//! pages lazily: the file can keep its high-water mark. This measures whether the
//! on-disk size follows the logical size, which decides whether pruning alone is
//! enough or a compaction step is required too.
//!
//! Run: `cargo run --release -p feather-core --example prune_probe`

use feather_core::message::Message;
use feather_core::snapshot::SnapshotStore;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = std::env::temp_dir().join(format!("feather-prune-probe-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("snapshots.redb");
    let store = SnapshotStore::open(&dir)?;

    // A deliberately fat history: every turn stores a full message list, and the
    // messages grow, so the snapshot store accumulates the same shape a real
    // long session does.
    let mut messages: Vec<Message> = Vec::new();
    let mut sizes = Vec::new();
    for turn in 1..=40u64 {
        for i in 0..4 {
            messages.push(Message::user_text(format!(
                "turn {turn} message {i} {}",
                "x".repeat(2000)
            )));
        }
        store.snapshot(turn, &messages)?;
        if turn % 10 == 0 {
            let bytes = std::fs::metadata(&path)?.len();
            let (stored, referenced) = store.chunk_census()?;
            sizes.push((turn, bytes, stored, referenced));
        }
    }

    println!("before pruning:");
    for (turn, bytes, stored, referenced) in &sizes {
        println!(
            "  turn {turn:>2}: {:>8.2} MiB  chunks={stored} referenced={referenced}",
            *bytes as f64 / (1024.0 * 1024.0)
        );
    }

    let before = std::fs::metadata(&path)?.len();
    let dropped = store.prune_to_last(10)?;
    let dead = store.vacuum()?;
    let after_vacuum = std::fs::metadata(&path)?.len();
    // Reopen with a fresh handle: redb may only report the shrunk size once the
    // process that grew it lets go.
    drop(store);
    let reopened = SnapshotStore::open(&dir)?;
    let (stored, referenced) = reopened.chunk_census()?;
    let after_reopen = std::fs::metadata(&path)?.len();

    println!(
        "\npruned {dropped} snapshots, vacated {dead} chunks\n\
         file: {:.2} MiB -> {:.2} MiB after vacuum -> {:.2} MiB after reopen\n\
         chunks: stored={stored} referenced={referenced}\n\
         remaining turns: {}",
        before as f64 / (1024.0 * 1024.0),
        after_vacuum as f64 / (1024.0 * 1024.0),
        after_reopen as f64 / (1024.0 * 1024.0),
        reopened.list()?.len()
    );
    drop(reopened);
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}
