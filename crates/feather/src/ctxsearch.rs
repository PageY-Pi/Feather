//! `search_context`: search this session's own history.
//!
//! The search itself lives in [`feather_core::ctxsearch`] so the tool and the
//! benchmark that measures it share one implementation. This module is the wire
//! adapter: schema, argument parsing and the model-facing rendering.
//!
//! Why the tool exists: compaction folds old turns into a summary, and a summary
//! is lossy. Without a way to read the raw audit back, "compacted" and "gone"
//! are the same thing to the model, so a detail the summarizer dropped can only
//! be guessed at. Every original message is still on disk — compaction never
//! writes to the raw audit — so the detail is recoverable.

use feather_core::ctxsearch::{search_session, ContextSearch, RoleFilter};
use feather_core::tool::{AgentTool, BoxFut, ToolOutcome};
use serde_json::Value;
use std::path::PathBuf;

pub struct SearchContextTool {
    /// The directory holding this session's `session.redb`.
    pub session_dir: PathBuf,
}

fn get_str(args: &Value, key: &str) -> Option<String> {
    args.get(key).and_then(|v| v.as_str()).map(str::to_string)
}

fn get_usize(args: &Value, key: &str) -> Option<usize> {
    args.get(key).and_then(|v| v.as_u64()).map(|v| v as usize)
}

/// Render a search as the model sees it.
fn render(found: &ContextSearch) -> String {
    if found.hits.is_empty() {
        return format!(
            "no match for {:?} in the {} most recent of {} stored messages",
            found.query, found.scanned, found.total
        );
    }
    let mut out = format!(
        "{} match(es) in the {} most recent of {} stored messages, newest first:\n",
        found.hits.len(),
        found.scanned,
        found.total
    );
    for hit in &found.hits {
        out.push_str(&format!(
            "\n[seq {} · {} · {} ms] {}\n",
            hit.seq, hit.role, hit.timestamp, hit.snippet
        ));
    }
    if found.more_may_exist {
        out.push_str("\n... more may exist further back; raise `scan`.\n");
    }
    out
}

impl AgentTool for SearchContextTool {
    fn name(&self) -> &'static str {
        "search_context"
    }

    fn description(&self) -> &'static str {
        "Search this session's full history, including turns compaction folded into a summary. \
Every original message is kept verbatim on disk; compaction removes text from what the model sees, never from storage. \
Use this to recover a path, command, error message or identifier that the summary dropped, or to check what was actually said earlier. \
Results are newest-first with the sequence number and timestamp of each hit."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Text to find. Matched case-insensitively against message text."
                },
                "limit": {
                    "type": "integer",
                    "description": "Max matching messages to return (default 20)"
                },
                "scan": {
                    "type": "integer",
                    "description": "How many recent messages to scan from newest backwards (default 2000). Raise it to reach further back."
                },
                "role": {
                    "type": "string",
                    "enum": ["user", "assistant", "tool"],
                    "description": "Only match messages from this role"
                }
            },
            "required": ["query"]
        })
    }

    fn parallel_safe(&self) -> bool {
        true
    }

    fn execute<'a>(
        &'a self,
        args: Value,
        _abort: tokio::sync::watch::Receiver<bool>,
    ) -> BoxFut<'a> {
        Box::pin(async move {
            let query = match get_str(&args, "query") {
                Some(q) if !q.trim().is_empty() => q,
                _ => return ToolOutcome::err("missing argument: query"),
            };
            let limit = get_usize(&args, "limit").unwrap_or(20).clamp(1, 100);
            let scan = get_usize(&args, "scan").unwrap_or(2000).clamp(1, 20_000);
            let role = match get_str(&args, "role") {
                None => RoleFilter::Any,
                Some(name) => match RoleFilter::parse(&name) {
                    Some(r) => r,
                    None => {
                        return ToolOutcome::err(format!(
                            "unknown role {name:?}; use user, assistant or tool"
                        ))
                    }
                },
            };
            match search_session(&self.session_dir, &query, role, limit, scan) {
                Ok(found) => ToolOutcome::ok(render(&found)),
                Err(e) => ToolOutcome::err(e.to_string()),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use feather_core::ctxsearch::ContextHit;

    fn hit(seq: u64) -> ContextHit {
        ContextHit {
            seq,
            role: "user",
            timestamp: 1_700_000_000_000,
            snippet: "found it".into(),
        }
    }

    #[test]
    fn render_lists_rows_and_scope() {
        let found = ContextSearch {
            query: "q".into(),
            hits: vec![hit(3), hit(1)],
            scanned: 10,
            total: 40,
            more_may_exist: false,
        };
        let text = render(&found);
        assert!(text.contains("2 match(es)"), "{text}");
        assert!(text.contains("10 most recent of 40 stored"), "{text}");
        assert!(text.contains("[seq 3 · user ·"), "{text}");
        assert!(!text.contains("raise `scan`"), "{text}");
    }

    #[test]
    fn render_reports_more_may_exist() {
        let found = ContextSearch {
            query: "q".into(),
            hits: vec![hit(3)],
            scanned: 2,
            total: 40,
            more_may_exist: true,
        };
        assert!(
            render(&found).contains("raise `scan`"),
            "{}",
            render(&found)
        );
    }

    #[test]
    fn render_says_when_nothing_matched() {
        let found = ContextSearch {
            query: "zzz".into(),
            hits: vec![],
            scanned: 5,
            total: 5,
            more_may_exist: false,
        };
        let text = render(&found);
        assert!(text.contains("no match"), "{text}");
        assert!(text.contains("5 stored messages"), "{text}");
    }
}
