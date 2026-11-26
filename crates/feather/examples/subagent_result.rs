//! Print what a subagent was finally asked to return.
//!
//! `subagent` truncates its result in the transcript, so a long review comes back
//! cut mid-sentence. The full record lives in the session's `subagents.redb`, which
//! is what this reads. `SubagentStore::list` finds the children; `transcript` gives
//! the recorded lines and `get` gives the final output.
//!
//! Run: `cargo run --release -p feather --example subagent_result -- <session-dir> [id-prefix]`

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let dir = args
        .next()
        .ok_or_else(|| anyhow::anyhow!("usage: subagent_result <session-dir> [id-prefix]"))?;
    let prefix = args.next();
    let store = feather_core::substore::SubagentStore::open(std::path::Path::new(&dir))?;
    let mut records = store.list()?;
    // Newest last, so the most recent child is the one printed when no id is given.
    records.sort_by_key(|r| r.id.clone());
    for record in records {
        if let Some(prefix) = prefix.as_ref() {
            if !record.id.starts_with(prefix.as_str()) {
                continue;
            }
        }
        println!("=== {} ===", record.id);
        // A child that timed out has an empty `output` but its messages are still
        // on disk, one per `push`. Printing the tail is what recovers work that
        // would otherwise look lost.
        if let Ok(messages) = store.messages(&record.id) {
            if !messages.is_empty() {
                println!("--- {} recorded messages (tail) ---", messages.len());
                let start = messages.len().saturating_sub(3);
                for m in &messages[start..] {
                    let text = m.text_cow();
                    let shown: String = text.chars().take(600).collect();
                    println!("[{:?}] {shown}", m.role);
                }
            }
        }
        if let Some(out) = store.get(&record.id)? {
            println!(
                "label: {}  model: {}  lines: {}  finished: {}",
                out.label,
                out.model,
                out.lines,
                out.finished_ms.is_some()
            );
            println!("--- output ---");
            println!("{}", out.output.unwrap_or_default());
            if let Some(error) = out.error {
                println!("--- error ---");
                println!("{error}");
            }
        }
    }
    Ok(())
}
