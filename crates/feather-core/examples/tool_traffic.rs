//! Measure how hashline tools actually behave in real sessions.
//!
//! Question: which tools fail, how often, and why? A tool that is rarely used or
//! frequently rejected is a candidate for removal or merger; a tool that is used
//! constantly and succeeds is not, regardless of how the schema looks.
//!
//! Reads every `session.redb` under the Feather home, so the answer comes from
//! real traffic rather than from reasoning about the schemas.

use feather_core::message::Role;
use feather_core::session_store::SessionStore;
use std::collections::BTreeMap;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let home = std::env::var_os("FEATHER_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| dirs_home().map(|h| h.join(".feather")))
        .ok_or("no Feather home")?;
    let sessions = home.join("sessions");
    let mut dirs: Vec<_> = std::fs::read_dir(&sessions)?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();

    // tool name -> (ok calls, error calls, error samples)
    let mut stats: BTreeMap<String, (u64, u64, Vec<String>)> = BTreeMap::new();
    let mut sessions_read = 0usize;
    let mut sessions_skipped = 0usize;

    for dir in &dirs {
        let store = match SessionStore::open_existing(dir) {
            Ok(Some(s)) => s,
            _ => {
                sessions_skipped += 1;
                continue;
            }
        };
        sessions_read += 1;
        let mut before = None;
        loop {
            let page = store.raw_page_before(before, 256, 8 << 20)?;
            for entry in &page.entries {
                let msg = &entry.message;
                if msg.role != Role::ToolResult {
                    continue;
                }
                let Some(name) = msg.tool_name.as_deref() else {
                    continue;
                };
                let slot = stats.entry(name.to_string()).or_default();
                if msg.is_error {
                    slot.1 += 1;
                    if slot.2.len() < 3 {
                        let text = msg.text();
                        let first = text.lines().next().unwrap_or("").trim();
                        slot.2.push(first.chars().take(160).collect());
                    }
                } else {
                    slot.0 += 1;
                }
            }
            match page.next_before {
                Some(next) => before = Some(next),
                None => break,
            }
        }
    }

    println!(
        "sessions: read {} / {} (skipped {} - locked or absent)",
        sessions_read,
        dirs.len(),
        sessions_skipped
    );
    println!();
    println!(
        "{:<20} {:>7} {:>7} {:>8}  {}",
        "tool", "ok", "error", "err%", "sample error"
    );
    let mut rows: Vec<_> = stats.iter().collect();
    rows.sort_by(|a, b| (b.1).0.cmp(&(a.1).0));
    for (name, (ok, err, samples)) in rows {
        let total = ok + err;
        let pct = if total == 0 {
            0.0
        } else {
            *err as f64 * 100.0 / total as f64
        };
        let sample = samples.first().map(String::as_str).unwrap_or("");
        println!(
            "{:<20} {:>7} {:>7} {:>7.1}%  {}",
            name, ok, err, pct, sample
        );
    }

    println!();
    println!("--- every distinct error, per tool ---");
    for (name, (ok, err, samples)) in &stats {
        if *err == 0 {
            continue;
        }
        println!("\n{name}: {err} errors of {}", ok + err);
        for s in samples {
            println!("    {s}");
        }
    }
    Ok(())
}

fn dirs_home() -> Option<std::path::PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(std::path::PathBuf::from)
}
