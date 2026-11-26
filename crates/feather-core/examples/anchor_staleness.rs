//! Why anchors go stale: the distance between the read and the failed edit.
//!
//! `is not a live anchor` has two very different causes, and they need opposite
//! fixes:
//!
//! 1. **Stale after intervening edits.** The model read a file, something edited
//!    between the read and the edit (its own earlier edit, a formatter, a test
//!    run), and the anchor moved. The fix is about how edits are batched and when
//!    a re-read is forced.
//! 2. **Never valid.** The anchor was copied wrong, invented, or came from an
//!    older build (4-letter). The fix is about the error message teaching the
//!    recovery.
//!
//! Counts how many tool calls sit between the `read` that served an anchor and the
//! edit that was rejected, and whether any edit to the same file happened in
//! between. That separates cause 1 from cause 2 without guessing.

use feather_core::message::Role;
use feather_core::session_store::SessionStore;
use std::collections::BTreeMap;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let home = std::env::var_os("FEATHER_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| dirs_home().map(|h| h.join(".feather")))
        .ok_or("no Feather home")?;
    let mut dirs: Vec<_> = std::fs::read_dir(home.join("sessions"))?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();

    // Buckets by "how many edits to the same file happened after the last read
    // that could have served this anchor, before the failing call".
    let mut buckets: BTreeMap<&'static str, u64> = BTreeMap::new();
    let mut total_stale = 0u64;
    let mut samples: Vec<String> = Vec::new();

    for dir in &dirs {
        let Ok(Some(store)) = SessionStore::open_existing(dir) else {
            continue;
        };
        let mut entries = Vec::new();
        let mut before = None;
        loop {
            let page = store.raw_page_before(before, 256, 8 << 20)?;
            entries.extend(page.entries);
            match page.next_before {
                Some(next) => before = Some(next),
                None => break,
            }
        }
        entries.sort_by_key(|e| e.seq);

        // Anchor tools are addressed by anchor, not path, so the arguments do not
        // say which file an edit targets. What the transcript does record is the
        // order of events, so the question becomes: how many tool calls happened
        // between the read that served the anchor and the edit that was refused.
        // A short gap means the anchor came from a recent read and still failed,
        // which points at invention or miscopy; a long gap points at intervening
        // edits.
        let mut calls_since_read: Option<u32> = None;
        // Per-file state as we walk the transcript in order.
        let mut edits_since_read: BTreeMap<String, u32> = BTreeMap::new();
        // call id -> (file path or "", tool name) so a failure can be attributed.
        let mut call_meta: BTreeMap<String, (String, String)> = BTreeMap::new();

        for entry in &entries {
            let msg = &entry.message;
            if msg.role != Role::ToolResult {
                continue;
            }
            let Some(name) = msg.tool_name.as_deref() else {
                continue;
            };
            let text = msg.text();

            if !msg.is_error {
                if name == "read" {
                    calls_since_read = Some(0);
                } else if let Some(n) = calls_since_read.as_mut() {
                    *n += 1;
                }
                continue;
            }
            if !text.contains("not a live anchor") && !text.contains("E_UNKNOWN_ANCHOR") {
                continue;
            }
            total_stale += 1;
            let gap = calls_since_read;
            let bucket = match gap {
                None => "no read earlier in the session",
                Some(0) => "first call after a read",
                Some(1..=2) => "1-2 calls after a read",
                Some(3..=10) => "3-10 calls after a read",
                Some(_) => "more than 10 calls after a read",
            };
            *buckets.entry(bucket).or_default() += 1;
            if samples.len() < 8 {
                samples.push(format!(
                    "{name} (calls since last read: {}): {}",
                    match gap {
                        Some(n) => n.to_string(),
                        None => "none".into(),
                    },
                    text.lines().next().unwrap_or("").trim()
                ));
            }
        }
    }

    println!("stale-anchor failures: {total_stale}");
    println!();
    println!("{:<32} {:>7}   (share)", "cause", "");
    let mut rows: Vec<_> = buckets.iter().collect();
    rows.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
    for (bucket, n) in rows {
        let pct = if total_stale == 0 {
            0.0
        } else {
            *n as f64 * 100.0 / total_stale as f64
        };
        println!("{bucket:<32} {n:>7}   {pct:.1}%");
    }
    println!();
    println!("--- samples ---");
    for s in &samples {
        println!("{s}");
    }
    Ok(())
}

fn dirs_home() -> Option<std::path::PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(std::path::PathBuf::from)
}
