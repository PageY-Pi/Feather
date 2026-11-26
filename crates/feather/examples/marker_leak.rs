//! Find where a tool-call marker leaked into assistant *text*.
//!
//! A model can emit its native tool-call syntax (`<|DSML|>`, `｜｜DSML｜｜`, a bare
//! `invoke name=...`) inside a content delta instead of a `tool_calls` field. Feather
//! has no detection for that, so the marker becomes visible assistant text and the
//! parsed "tool name" becomes whatever prose happened to precede it — which is how
//! `unknown tool: constraints>` and `unknown tool: grep` appeared in the traffic report.
//!
//! This walks the stored messages and prints every occurrence with enough context to
//! tell the two cases apart: marker in `Text` (a real leak, the model tried to call a
//! tool in prose) versus marker inside `Thinking` (harmless, the reasoning was aired).

use feather_core::message::{ContentBlock, Role};
use feather_core::session_store::SessionStore;
use std::path::PathBuf;

const MARKERS: [&str; 4] = ["DSML", "｜｜DSML｜｜", "invoke name=", "antml:invoke"];

fn main() -> anyhow::Result<()> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let dirs: Vec<PathBuf> = if args.is_empty() {
        let home = feather::config::feather_home();
        std::fs::read_dir(home.join("sessions"))?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.join("session.redb").exists())
            .collect()
    } else {
        args.into_iter().map(PathBuf::from).collect()
    };

    let mut leaks = 0usize;
    let mut thinking_only = 0usize;

    for dir in dirs {
        let store = match SessionStore::open(&dir) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("{dir:?}: cannot open: {e}");
                continue;
            }
        };
        let messages = match store.raw_messages() {
            Ok(m) => m,
            Err(e) => {
                eprintln!("{dir:?}: cannot read messages: {e}");
                continue;
            }
        };
        for (i, m) in messages.iter().enumerate() {
            if m.role != Role::Assistant {
                continue;
            }
            for b in &m.content {
                let (body, in_text) = match b {
                    ContentBlock::Text { text } => (text.as_str(), true),
                    ContentBlock::Thinking { text, .. } => (text.as_str(), false),
                    _ => continue,
                };
                let hit = MARKERS.iter().find(|m| body.contains(**m));
                let Some(marker) = hit else { continue };
                let at = body.find(marker).unwrap_or(0);
                let start = at.saturating_sub(160);
                let end = (at + 320).min(body.len());
                // Chars, not bytes: the marker itself is non-ASCII in one form.
                let snippet = &body[start..end];
                if in_text {
                    leaks += 1;
                    println!(
                        "LEAK {} msg#{i} marker={marker:?}\n  ...{}...\n",
                        dir.file_name().and_then(|n| n.to_str()).unwrap_or("?"),
                        snippet.replace('\n', "\n  ")
                    );
                } else {
                    thinking_only += 1;
                }
            }
        }
    }

    println!("\n{leaks} leak(s) in assistant text, {thinking_only} occurrence(s) only in thinking");
    if leaks == 0 && thinking_only > 0 {
        println!("The marker appears only in reasoning, so it never reached a tool name.");
    }
    Ok(())
}
