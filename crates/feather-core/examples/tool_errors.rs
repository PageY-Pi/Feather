//! Classify how hashline tools fail in real sessions.
//!
//! Two failures look similar in a percentage and have nothing in common:
//!
//! - **missing argument** — the model did not fill a required field. This is a
//!   schema/ergonomics problem: fewer required fields, or one tool instead of two.
//! - **stale anchor** — the model filled everything and the anchor had moved. This
//!   is the protocol working: the edit is refused before it corrupts anything.
//!
//! The first is a defect worth redesigning for. The second is the cost of
//! correctness, and merging tools would not reduce it. Measuring them together
//! makes the tool look worse than it is and points the fix at the wrong thing.

use feather_core::message::Role;
use feather_core::session_store::SessionStore;
use std::collections::BTreeMap;

#[derive(Default, Clone, Copy)]
struct Counts {
    ok: u64,
    missing_arg: u64,
    stale_anchor: u64,
    range_order: u64,
    undo_stale: u64,
    other: u64,
}

impl Counts {
    fn total(&self) -> u64 {
        self.ok
            + self.missing_arg
            + self.stale_anchor
            + self.range_order
            + self.undo_stale
            + self.other
    }
}

/// Which failure this error text represents.
///
/// The classification reads the message the tool itself produced, so it stays
/// accurate when a tool changes its wording only insofar as the wording is
/// stable — each branch names the exact prefix it matches.
fn classify(text: &str, is_error: bool) -> &'static str {
    if !is_error {
        return "ok";
    }
    if text.contains("missing argument") || text.contains("provide status and/or title") {
        "missing_arg"
    } else if text.contains("E_UNKNOWN_ANCHOR") || text.contains("is not a live anchor") {
        "stale_anchor"
    } else if text.contains("E_RANGE_ORDER") {
        "range_order"
    } else if text.contains("E_UNDO_STALE") {
        "undo_stale"
    } else {
        "other"
    }
}

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

    let mut stats: BTreeMap<String, Counts> = BTreeMap::new();
    let mut other_samples: BTreeMap<String, Vec<String>> = BTreeMap::new();

    for dir in &dirs {
        let Ok(Some(store)) = SessionStore::open_existing(dir) else {
            continue;
        };
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
                let text = msg.text();
                let slot = stats.entry(name.to_string()).or_default();
                match classify(&text, msg.is_error) {
                    "ok" => slot.ok += 1,
                    "missing_arg" => slot.missing_arg += 1,
                    "stale_anchor" => slot.stale_anchor += 1,
                    "range_order" => slot.range_order += 1,
                    "undo_stale" => slot.undo_stale += 1,
                    _ => {
                        slot.other += 1;
                        let v = other_samples.entry(name.to_string()).or_default();
                        if v.len() < 12 {
                            let first = text.lines().next().unwrap_or("").trim();
                            v.push(first.chars().take(90).collect());
                        }
                    }
                }
            }
            match page.next_before {
                Some(next) => before = Some(next),
                None => break,
            }
        }
    }

    println!(
        "{:<16} {:>6} {:>6} {:>7} {:>6} {:>6} {:>6} {:>6}  {:>7}",
        "tool", "total", "ok", "missing", "stale", "range", "undo", "other", "fail%"
    );
    let mut rows: Vec<_> = stats.iter().collect();
    rows.sort_by(|a, b| b.1.total().cmp(&a.1.total()));
    for (name, c) in rows {
        let total = c.total();
        let fail = total - c.ok;
        let pct = if total == 0 {
            0.0
        } else {
            fail as f64 * 100.0 / total as f64
        };
        println!(
            "{:<16} {:>6} {:>6} {:>7} {:>6} {:>6} {:>6} {:>6}  {:>6.1}%",
            name,
            total,
            c.ok,
            c.missing_arg,
            c.stale_anchor,
            c.range_order,
            c.undo_stale,
            c.other,
            pct
        );
    }

    println!();
    println!("--- non-argument, non-anchor failures (2 samples each) ---");
    for (name, samples) in &other_samples {
        println!("{name}:");
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
