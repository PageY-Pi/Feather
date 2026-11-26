//! What does the model actually send when it gets a hashline edit wrong?
//!
//! A missing-argument failure is only actionable if you can see what was sent
//! instead. Was the field omitted entirely, or sent under a plausible wrong name
//! (`from` vs `replace_from`)? Those need different fixes: a schema that is too
//! wide, versus one whose field names do not match what a model reaches for.
//!
//! Correlates each failed tool result with the assistant tool call that produced
//! it, by `tool_call_id`, and dumps the arguments.

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

    // Argument keys the model sent when the call failed for a missing field.
    let mut key_sets: BTreeMap<String, u64> = BTreeMap::new();
    // Sessions where the model sent `replace` arguments to `replace_within`.
    let mut confused_sessions: BTreeMap<String, u64> = BTreeMap::new();
    let mut by_model: BTreeMap<String, u64> = BTreeMap::new();
    let mut samples: Vec<String> = Vec::new();

    for dir in &dirs {
        let Ok(Some(store)) = SessionStore::open_existing(dir) else {
            continue;
        };
        // id -> (name, arguments) from assistant calls.
        let mut calls: BTreeMap<String, (String, String)> = BTreeMap::new();
        let mut model = String::from("?");
        let mut before = None;
        // Newest-first, so collect the whole page chain then process in order.
        let mut entries = Vec::new();
        loop {
            let page = store.raw_page_before(before, 256, 8 << 20)?;
            entries.extend(page.entries);
            match page.next_before {
                Some(next) => before = Some(next),
                None => break,
            }
        }
        entries.sort_by_key(|e| e.seq);
        for entry in &entries {
            let msg = &entry.message;
            if msg.model.is_some() {
                if let Some(m) = &msg.model {
                    model = m.clone();
                }
            }
            for call in msg.iter_tool_calls() {
                calls.insert(
                    call.id.clone(),
                    (call.name.clone(), call.arguments.to_string()),
                );
            }
        }
        for entry in &entries {
            let msg = &entry.message;
            if msg.role != Role::ToolResult || !msg.is_error {
                continue;
            }
            let text = msg.text();
            if !text.contains("missing string argument") {
                continue;
            }
            let Some(id) = msg.tool_call_id.as_deref() else {
                continue;
            };
            let Some((name, args)) = calls.get(id) else {
                continue;
            };
            *by_model.entry(format!("{name} on {model}")).or_default() += 1;
            if name == "replace_within" && keys_have_replace_shape(args) {
                *confused_sessions
                    .entry(
                        dir.file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .to_string(),
                    )
                    .or_default() += 1;
            }
            // The key set the model sent, sorted, so the same mistake groups.
            let keys: Vec<String> = serde_json::from_str::<serde_json::Value>(args)
                .ok()
                .and_then(|v| v.as_object().map(|o| o.keys().cloned().collect()))
                .unwrap_or_default();
            *key_sets
                .entry(format!("{name} -> [{}]", keys.join(", ")))
                .or_default() += 1;
            if samples.len() < 6 {
                let short: String = args.chars().take(220).collect();
                samples.push(format!("{name}({short})"));
            }
        }
    }

    println!("=== missing-field failures: which keys were actually sent ===");
    let mut rows: Vec<_> = key_sets.iter().collect();
    rows.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
    for (desc, n) in rows.iter().take(20) {
        println!("{:>6}  {desc}", n);
    }

    println!();
    println!("=== is the confusion systemic, or one stuck session? ===");
    let mut cs: Vec<_> = confused_sessions.iter().collect();
    cs.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
    let total: u64 = cs.iter().map(|(_, n)| **n).sum();
    println!(
        "{total} replace-shaped calls to replace_within, in {} session(s)",
        cs.len()
    );
    for (sess, n) in cs.iter().take(10) {
        println!("    {sess}  {n}");
    }
    println!();
    println!("=== by tool and model ===");
    let mut m: Vec<_> = by_model.iter().collect();
    m.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
    for (desc, n) in m.iter().take(15) {
        println!("{:>6}  {desc}", n);
    }

    println!();
    println!("=== raw arguments of failing calls ===");
    for s in &samples {
        println!("{s}");
    }
    Ok(())
}

/// True when the arguments look like `replace`'s schema, not `replace_within`'s.
///
/// This is the signature of tool-name confusion rather than a hard schema: the
/// model reached for the right shape of edit and picked the wrong verb.
fn keys_have_replace_shape(args: &str) -> bool {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(args) else {
        return false;
    };
    let Some(o) = v.as_object() else { return false };
    o.contains_key("remove_from")
        || o.contains_key("remove_to")
        || o.contains_key("replacement_lines")
}
fn dirs_home() -> Option<std::path::PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(std::path::PathBuf::from)
}
