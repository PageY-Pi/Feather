//! Why `subagent` calls fail.
//!
//! 64 of 88 real calls failed, which is the worst rate of any tool. The receipt
//! text distinguishes the causes but each failure reports `[status=error ...]`
//! first, so a naive count sees only "error". This buckets by the message that
//! follows the receipt header, and correlates with the arguments the model sent.
//!
//! Failure modes the code can produce, per `SubagentTool::execute`:
//!   * `subagent: timed out`
//!   * `subagent: aborted`
//!   * `subagent: provider stream failed`
//!   * `subagent: durable record failed`
//!   * an arbitrary `Err` string from the inner run
//! so the question is which of these actually happens, and whether the model's
//! arguments (model name, task length) predict it.

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

    let mut buckets: BTreeMap<String, u64> = BTreeMap::new();
    // model argument -> (ok, error), to see if one model is unusable.
    let mut by_model: BTreeMap<String, (u64, u64)> = BTreeMap::new();
    let mut samples: Vec<String> = Vec::new();
    // timeoutMs actually sent -> number of timeouts.
    let mut timeouts: BTreeMap<String, u64> = BTreeMap::new();
    let mut ok_texts: Vec<String> = Vec::new();

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

        // call id -> model argument
        let mut call_model: BTreeMap<String, String> = BTreeMap::new();
        for entry in &entries {
            for call in entry.message.iter_tool_calls() {
                if call.name != "subagent" {
                    continue;
                }
                let model = call
                    .arguments
                    .get("model")
                    .and_then(|v| v.as_str())
                    .unwrap_or("(default)")
                    .to_string();
                let keys: Vec<&str> = call
                    .arguments
                    .as_object()
                    .map(|o| o.keys().map(String::as_str).collect())
                    .unwrap_or_default();
                let timeout = call
                    .arguments
                    .get("timeoutMs")
                    .and_then(|v| v.as_u64())
                    .map(|ms| format!("{ms}ms"))
                    .unwrap_or_else(|| "default(300000ms)".into());
                let task_len = call
                    .arguments
                    .get("task")
                    .and_then(|v| v.as_str())
                    .map(str::len)
                    .unwrap_or(0);
                call_model.insert(
                    call.id.clone(),
                    format!("{model} keys={keys:?} timeout={timeout} task_len={task_len}"),
                );
            }
        }

        for entry in &entries {
            let msg = &entry.message;
            if msg.role != Role::ToolResult || msg.tool_name.as_deref() != Some("subagent") {
                continue;
            }
            let text = msg.text();
            let model = msg
                .tool_call_id
                .as_deref()
                .and_then(|id| call_model.get(id))
                .cloned()
                .unwrap_or_else(|| "(unmatched)".into());
            let slot = by_model.entry(model.clone()).or_default();
            if !msg.is_error {
                slot.0 += 1;
                if ok_texts.len() < 3 {
                    ok_texts.push(text.lines().take(3).collect::<Vec<_>>().join(" / "));
                }
                continue;
            }
            slot.1 += 1;

            // Classify on the body after the receipt header, which is the line
            // that actually says what went wrong.
            let body: Vec<&str> = text
                .lines()
                .filter(|l| !l.starts_with("[status="))
                .collect();
            let first = body.first().copied().unwrap_or("").trim();
            let kind = if text.contains("timed out") {
                "timed out"
            } else if text.contains("aborted") {
                "aborted"
            } else if text.contains("provider stream failed") {
                "provider stream failed"
            } else if text.contains("durable record failed") {
                "durable record failed"
            } else if text.contains("circular") || text.contains("recursion") {
                "recursion"
            } else if first.is_empty() {
                "(empty body)"
            } else {
                first
            };
            let kind = kind.chars().take(120).collect::<String>();
            *buckets.entry(kind.clone()).or_default() += 1;
            // Timeouts need their own accounting: the fix differs completely
            // depending on whether the model set a short deadline or the default
            // was too short for the work it asked for.
            if text.contains("timed out") {
                let t = model.split(" timeout=").nth(1).unwrap_or("?").to_string();
                let t = t.split(' ').next().unwrap_or("?").to_string();
                *timeouts.entry(t).or_default() += 1;
            }
            if samples.len() < 12 {
                samples.push(format!("[{model}] {kind}"));
            }
        }
    }

    println!("=== subagent failure causes ===");
    let mut rows: Vec<_> = buckets.iter().collect();
    rows.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
    let total: u64 = rows.iter().map(|(_, n)| **n).sum();
    for (kind, n) in rows {
        let pct = if total == 0 {
            0.0
        } else {
            *n as f64 * 100.0 / total as f64
        };
        println!("{n:>6}  {pct:>5.1}%  {kind}");
    }
    println!("{total:>6}         total failures");

    println!();
    println!("=== timeouts, by the timeoutMs that was sent ===");
    let mut t: Vec<_> = timeouts.iter().collect();
    t.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
    for (ms, n) in t {
        println!("{n:>6}  {ms}");
    }

    println!();
    println!("=== by model argument (ok / error) ===");
    let mut m: Vec<_> = by_model.iter().collect();
    m.sort_by_key(|(_, (a, b))| std::cmp::Reverse(a + b));
    for (model, (ok, err)) in m {
        println!("{ok:>4} ok {err:>4} err   {model}");
    }

    println!();
    println!("=== first failures verbatim ===");
    for s in &samples {
        println!("{s}");
    }
    println!();
    println!("=== what a success looks like ===");
    for s in &ok_texts {
        println!("{s}");
    }
    Ok(())
}

fn dirs_home() -> Option<std::path::PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(std::path::PathBuf::from)
}
