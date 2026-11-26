//! Why hashline edits fail: exact taxonomy, and whether it is concentrated.
//!
//! A 42% failure rate on one tool means one of two very different things:
//!
//! 1. The tool is hard to use, and the rate is spread evenly across sessions.
//! 2. A handful of sessions got stuck in a retry loop, and the rate is an
//!    artefact of one pathology counted thousands of times.
//!
//! Only the first justifies redesigning the tool. The second justifies fixing the
//! loop. This separates them before anything is changed.

use feather_core::message::Role;
use feather_core::session_store::SessionStore;
use std::collections::BTreeMap;

/// The precise reason, because "other" hid the answer the first time.
fn classify(text: &str, is_error: bool) -> &'static str {
    if !is_error {
        return "ok";
    }
    if text.contains("missing string argument") || text.contains("missing argument") {
        "missing_arg"
    } else if text.contains("E_ANCHOR_FORMAT") {
        "anchor_format"
    } else if text.contains("E_MATCH_COUNT") {
        if text.contains("found 0") {
            "old_not_found"
        } else {
            "old_ambiguous"
        }
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

const KINDS: [&str; 9] = [
    "ok",
    "missing_arg",
    "anchor_format",
    "old_not_found",
    "old_ambiguous",
    "stale_anchor",
    "range_order",
    "undo_stale",
    "other",
];

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

    // tool -> kind -> count
    let mut overall: BTreeMap<String, BTreeMap<&'static str, u64>> = BTreeMap::new();
    // (tool, session) -> failures, to find concentration
    let mut per_session: BTreeMap<(String, String), u64> = BTreeMap::new();
    let mut other_heads: BTreeMap<String, u64> = BTreeMap::new();

    for dir in &dirs {
        let Ok(Some(store)) = SessionStore::open_existing(dir) else {
            continue;
        };
        let label = dir
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("?")
            .to_string();
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
                let kind = classify(&msg.text(), msg.is_error);
                if kind == "other" && msg.is_error {
                    let head: String = msg
                        .text()
                        .lines()
                        .next()
                        .unwrap_or("")
                        .trim()
                        .chars()
                        .take(70)
                        .collect();
                    *other_heads.entry(head).or_default() += 1;
                }
                *overall
                    .entry(name.to_string())
                    .or_default()
                    .entry(kind)
                    .or_default() += 1;
                if kind != "ok" {
                    *per_session
                        .entry((name.to_string(), label.clone()))
                        .or_default() += 1;
                    // no-op: per_session already records the session
                }
            }
            match page.next_before {
                Some(next) => before = Some(next),
                None => break,
            }
        }
    }

    println!("=== failure taxonomy ===");
    println!(
        "{:<16} {:>7} {:>7} {:>9} {:>10} {:>9} {:>7} {:>6}",
        "tool", "ok", "missing", "fmt", "not_found", "ambiguous", "stale", "other"
    );
    let mut rows: Vec<_> = overall.iter().collect();
    rows.sort_by_key(|(_, k)| std::cmp::Reverse(k.values().sum::<u64>()));
    for (name, kinds) in rows {
        let get = |k: &str| kinds.get(k).copied().unwrap_or(0);
        println!(
            "{:<16} {:>7} {:>7} {:>9} {:>10} {:>9} {:>7} {:>6}",
            name,
            get("ok"),
            get("missing_arg"),
            get("anchor_format"),
            get("old_not_found"),
            get("old_ambiguous"),
            get("stale_anchor"),
            get("other")
        );
    }

    println!();
    println!("=== where the failures live (sessions with >=20) ===");
    let mut grouped: BTreeMap<String, Vec<(String, u64)>> = BTreeMap::new();
    for ((tool, sess), n) in &per_session {
        if *n >= 20 {
            grouped
                .entry(tool.clone())
                .or_default()
                .push((sess.clone(), *n));
        }
    }
    for (tool, mut list) in grouped {
        list.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
        let total: u64 = per_session
            .iter()
            .filter(|((t, _), _)| *t == tool)
            .map(|(_, n)| n)
            .sum();
        println!(
            "{tool}: {total} failures across {} session(s) with >=20",
            list.len()
        );
        for (sess, n) in list.iter().take(6) {
            println!("    {sess}  {n}");
        }
    }
    println!();
    println!("=== the unclassified failures, verbatim (top 15) ===");
    let mut heads: Vec<_> = other_heads.iter().collect();
    heads.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
    for (head, n) in heads.iter().take(15) {
        println!("{:>6}  {head}", n);
    }
    Ok(())
}

fn dirs_home() -> Option<std::path::PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(std::path::PathBuf::from)
}
