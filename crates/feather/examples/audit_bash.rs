//! Local, read-only analysis of shell calls for the current workspace.
use std::collections::BTreeMap;
fn main() -> anyhow::Result<()> {
    let cwd = std::env::current_dir()?;
    let home = feather::config::feather_home();
    let sessions = feather_core::session_catalog::list_sessions(&home, &cwd)?;
    let mut total = 0;
    let mut failed = 0;
    let mut missing = 0;
    let mut patterns = BTreeMap::<String, usize>::new();
    println!("Workspace sessions: {}", sessions.len());
    for info in sessions {
        let store = feather_core::session_store::SessionStore::open(&info.dir)?;
        let mut calls = BTreeMap::new();
        let messages = store.raw_messages()?;
        for message in &messages {
            for call in message.tool_calls() {
                if call.name == "bash" {
                    calls.insert(
                        call.id.clone(),
                        call.arguments["command"]
                            .as_str()
                            .unwrap_or_default()
                            .to_string(),
                    );
                }
            }
        }
        for message in &messages {
            if let Some(command) = message
                .tool_call_id
                .as_ref()
                .and_then(|id| calls.remove(id))
            {
                total += 1;
                if message.is_error {
                    failed += 1;
                    let text = message.text();
                    let category = if text.contains("spawn failed") {
                        "spawn"
                    } else if text.contains("timed out") {
                        "timeout"
                    } else if text.contains("aborted") {
                        "abort"
                    } else {
                        "command exit"
                    };
                    *patterns.entry(category.into()).or_default() += 1;
                    // Only print commands and bounded error text, never request bodies/config files.
                    println!(
                        "FAIL session={} category={} command={}",
                        info.id,
                        category,
                        command
                            .replace('\n', " ")
                            .chars()
                            .take(220)
                            .collect::<String>()
                    );
                    println!(
                        "  {}",
                        text.lines()
                            .take(5)
                            .collect::<Vec<_>>()
                            .join(" | ")
                            .chars()
                            .take(500)
                            .collect::<String>()
                    );
                }
            }
        }
        missing += calls.len();
    }
    println!(
        "Bash results: total={total} failed={failed} pending={missing}; categories={patterns:?}"
    );
    Ok(())
}
