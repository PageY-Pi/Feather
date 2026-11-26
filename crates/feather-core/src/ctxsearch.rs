//! Search a session's raw audit.
//!
//! Compaction folds old turns into a summary, and a summary is lossy by
//! construction: a path, a command, an error string or an identifier can be
//! dropped from the live context. The original messages are never dropped from
//! storage — [`crate::session_store`] keeps every raw message verbatim in
//! `session.redb`, and compaction never writes there.
//!
//! This module is the reader for that audit. It lives in the kernel rather than
//! in the tool wrapper so the `search_context` tool and the benchmark that
//! measures it exercise one implementation, not two that can drift apart.

use crate::message::Role;
use crate::session_store::SessionStore;
use std::path::Path;

/// Which role to match, if the caller narrowed it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoleFilter {
    Any,
    User,
    Assistant,
    Tool,
}

impl RoleFilter {
    /// Parse the wire name used by the tool schema.
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "user" => Some(Self::User),
            "assistant" => Some(Self::Assistant),
            "tool" => Some(Self::Tool),
            "any" | "" => Some(Self::Any),
            _ => None,
        }
    }

    fn matches(self, role: Role) -> bool {
        match self {
            Self::Any => true,
            Self::User => role == Role::User,
            Self::Assistant => role == Role::Assistant,
            Self::Tool => role == Role::ToolResult,
        }
    }
}

/// One matched message, reduced to the region around the match.
#[derive(Debug, Clone, PartialEq)]
pub struct ContextHit {
    pub seq: u64,
    pub role: &'static str,
    pub timestamp: u64,
    /// The matched window, single-line and bounded.
    pub snippet: String,
}

/// What a search found, plus enough scope to explain a thin result.
#[derive(Debug, Clone, PartialEq)]
pub struct ContextSearch {
    pub query: String,
    pub hits: Vec<ContextHit>,
    /// Messages examined, newest-first from the end of the audit.
    pub scanned: usize,
    /// Total messages stored for this session.
    pub total: u64,
    /// True when `scan` stopped the walk before the start of the audit, so more
    /// matches may exist further back.
    pub more_may_exist: bool,
}

/// Role name as the model sees it.
fn role_name(role: Role) -> &'static str {
    match role {
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::ToolResult => "tool",
        Role::System => "system",
    }
}

/// Case-insensitive search for `needle`, returning a bounded window around the
/// first match with a little context on each side.
///
/// The window is measured in characters and sliced on char boundaries: session
/// text routinely contains CJK and emoji, and a byte-index slice would panic or
/// land mid-codepoint.
pub fn window_around(haystack: &str, needle_lower: &str, radius: usize) -> Option<String> {
    let lower = haystack.to_lowercase();
    let at = lower.find(needle_lower)?;
    // The byte offset is valid in the lowercased copy. Lowercasing can change
    // byte lengths for some scripts, so do not slice the original by that
    // offset; instead count chars before the match, which is stable under casing.
    let chars_before = lower[..at].chars().count();
    let match_chars = needle_lower.chars().count();
    let chars: Vec<(usize, char)> = haystack.char_indices().collect();
    let start_idx = chars_before.min(chars.len());
    let end_idx = (start_idx + match_chars).min(chars.len());
    let from_idx = start_idx.saturating_sub(radius);
    let to_idx = (end_idx + radius).min(chars.len());
    let mut out = String::new();
    if from_idx > 0 {
        out.push('…');
    }
    let begin = chars.get(from_idx).map(|(b, _)| *b).unwrap_or(0);
    let stop = chars.get(to_idx).map(|(b, _)| *b).unwrap_or(haystack.len());
    out.push_str(&haystack[begin..stop]);
    if to_idx < chars.len() {
        out.push('…');
    }
    Some(out.split_whitespace().collect::<Vec<_>>().join(" "))
}

/// The text of a message as search should see it.
///
/// Assistant tool calls carry their arguments as structured JSON, so a search
/// Case-insensitive substring search without allocating.
///
/// `needle` must already be lowercase. The ASCII case is handled by comparing
/// bytes with `eq_ignore_ascii_case`, so the common path touches no heap. Only a
/// non-ASCII haystack or needle falls back to full Unicode casing, which is
/// correct but allocates — and a session's text is overwhelmingly ASCII.
fn contains_ignore_case(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    if haystack.is_ascii() && needle.is_ascii() {
        let h = haystack.as_bytes();
        let n = needle.as_bytes();
        if n.len() > h.len() {
            return false;
        }
        return h.windows(n.len()).any(|w| w.eq_ignore_ascii_case(n));
    }
    haystack.to_lowercase().contains(needle)
}

/// Cheap presence test: does this message contain `needle` at all?
///
/// A scan examines hundreds of messages to return a handful, so the rejection
/// path is the hot path. This avoids materialising a searchable string for
/// messages that cannot match. It is a conservative pre-filter: `true` means
/// "worth building the string and looking properly", never "definitely a match".
fn matches_message(message: &crate::message::Message, needle_lower: &str) -> bool {
    // `text_cow` borrows for the single-text-block shape every chat row has.
    if contains_ignore_case(&message.text_cow(), needle_lower) {
        return true;
    }
    // Tool-call arguments are structured JSON and are not part of `text_cow`, so
    // they have to be checked separately.
    for call in message.iter_tool_calls() {
        if contains_ignore_case(&call.name, needle_lower) {
            return true;
        }
        if contains_ignore_case(&call.arguments.to_string(), needle_lower) {
            return true;
        }
    }
    false
}

/// The text of a message as search should see it.
///
/// Assistant tool calls carry their arguments as structured JSON, so a search
/// for a path or command would miss them if only the text block were read.
fn searchable_text(message: &crate::message::Message) -> String {
    if message.role != Role::Assistant {
        return message.text();
    }
    let mut s = message.text();
    for call in message.iter_tool_calls() {
        s.push(' ');
        s.push_str(&call.name);
        s.push(' ');
        s.push_str(&call.arguments.to_string());
    }
    s
}

/// Errors a search can report without inventing a result.
#[derive(Debug)]
pub enum SearchError {
    /// The session directory holds no `session.redb` yet.
    NoHistory,
    /// Storage refused the read.
    Store(String),
}

impl std::fmt::Display for SearchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoHistory => write!(
                f,
                "no session history on disk yet; nothing has been written for this session"
            ),
            Self::Store(e) => {
                // A lock conflict is not a storage fault: another process holds
                // the session. Saying that plainly is the difference between a
                // user retrying and a user thinking the session is corrupt.
                if e.contains("Cannot acquire lock") || e.contains("already open") {
                    write!(
                        f,
                        "this session is open in another feather process; \
                         close it there or search from the session that is running"
                    )
                } else {
                    write!(f, "cannot read session history: {e}")
                }
            }
        }
    }
}

/// Search `session_dir`'s raw audit, newest-first.
///
/// `scan` bounds how many recent messages are examined so a query that matches
/// recent turns does not pay for the whole audit; `limit` bounds the hits.
pub fn search_session(
    session_dir: &Path,
    query: &str,
    role: RoleFilter,
    limit: usize,
    scan: usize,
) -> Result<ContextSearch, SearchError> {
    let store = SessionStore::open_existing(session_dir)
        .map_err(|e| SearchError::Store(e.to_string()))?
        .ok_or(SearchError::NoHistory)?;
    search_store(&store, query, role, limit, scan)
}

/// Same search against an already-open store.
pub fn search_store(
    store: &SessionStore,
    query: &str,
    role: RoleFilter,
    limit: usize,
    scan: usize,
) -> Result<ContextSearch, SearchError> {
    let needle = query.to_lowercase();
    let mut hits: Vec<ContextHit> = Vec::new();
    let mut cursor: Option<u64> = None;
    let mut scanned = 0usize;
    let mut total = 0u64;
    let mut more_may_exist = false;

    while scanned < scan {
        // Page in bounded chunks: a huge audit must not be materialised whole,
        // and a match near the end must not read the start.
        let want = (scan - scanned).min(500);
        let page = store
            .raw_page_before(cursor, want, 8 << 20)
            .map_err(|e| SearchError::Store(e.to_string()))?;
        total = page.total;
        if page.entries.is_empty() {
            break;
        }
        // Enumerated so a `limit` stop can tell how many rows it never looked
        // at, which is what `more_may_exist` has to report honestly.
        let page_rows = page.entries.len();
        for (row_index, entry) in page.entries.iter().rev().enumerate() {
            let rows_left_in_page = page_rows - 1 - row_index;
            if scanned >= scan {
                more_may_exist = true;
                break;
            }
            scanned += 1;
            if !role.matches(entry.message.role) {
                continue;
            }
            // Cheap rejection first: most messages do not match, and building a
            // searchable string (plus its lowercased copy) for each of them is
            // the dominant cost of a scan. `text_cow` borrows when the message
            // has a single text block, which is the common shape.
            if !matches_message(&entry.message, &needle) {
                continue;
            }
            if let Some(snippet) = window_around(&searchable_text(&entry.message), &needle, 160) {
                hits.push(ContextHit {
                    seq: entry.seq,
                    role: role_name(entry.message.role),
                    timestamp: entry.timestamp,
                    snippet,
                });
                if hits.len() >= limit {
                    // Stopping here leaves rows unexamined in two places: the
                    // rest of this page, and any older page. Both count.
                    more_may_exist = rows_left_in_page > 0 || page.next_before.is_some();
                    break;
                }
            }
        }
        if hits.len() >= limit || scanned >= scan {
            break;
        }
        match page.next_before {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }
    if scanned >= scan && hits.len() < limit && total > scanned as u64 {
        more_may_exist = true;
    }

    Ok(ContextSearch {
        query: query.to_string(),
        hits,
        scanned,
        total,
        more_may_exist,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A lock conflict must read as "another process has it", not as corruption.
    #[test]
    fn lock_conflicts_explain_themselves() {
        let e = SearchError::Store(
            "redb error: Database already open. Cannot acquire lock.".to_string(),
        );
        let text = e.to_string();
        assert!(text.contains("another feather process"), "{text}");
        assert!(
            !text.contains("redb"),
            "internal detail must not leak: {text}"
        );

        // A genuine storage fault still reports the underlying reason.
        let other = SearchError::Store("disk on fire".to_string());
        assert!(other.to_string().contains("disk on fire"), "{other}");
    }

    /// The pre-filter must never reject a message the real search would match.
    ///
    /// It is the hot path's early exit, so a false negative here would silently
    /// hide history — the exact failure the tool exists to prevent.
    #[test]
    fn the_pre_filter_agrees_with_the_full_search() {
        use crate::message::{Api, ContentBlock, Message, StopReason, Usage};
        let needle = "needle";
        let cases = vec![
            Message::user_text("plain text with a needle inside"),
            Message::user_text("UPPER NEEDLE"),
            Message::user_text("羽毛 needle 中文"),
            Message::user_text("no match here"),
            Message::assistant(
                vec![ContentBlock::Text {
                    text: "no text match".into(),
                }],
                "m",
                Api::Faux,
                Usage::default(),
                StopReason::ToolUse,
            ),
        ];
        for msg in &cases {
            let prefiltered = matches_message(msg, needle);
            let authoritative = window_around(&searchable_text(msg), needle, 10).is_some();
            assert_eq!(
                prefiltered,
                authoritative,
                "pre-filter disagreed on {:?}: prefilter={prefiltered} full={authoritative}",
                msg.text()
            );
        }
    }

    #[test]
    fn role_filter_parses_wire_names() {
        assert_eq!(RoleFilter::parse("user"), Some(RoleFilter::User));
        assert_eq!(RoleFilter::parse("TOOL"), Some(RoleFilter::Tool));
        assert_eq!(RoleFilter::parse("assistant"), Some(RoleFilter::Assistant));
        assert_eq!(RoleFilter::parse("any"), Some(RoleFilter::Any));
        assert_eq!(RoleFilter::parse("system"), None);
    }

    #[test]
    fn window_keeps_the_match_visible() {
        let hay = "a".repeat(400) + " NEEDLE " + &"b".repeat(400);
        let w = window_around(&hay, "needle", 20).unwrap();
        assert!(w.to_lowercase().contains("needle"));
        assert!(w.starts_with('…') && w.ends_with('…'));
        assert!(w.chars().count() < 120, "window must stay small: {w:?}");
    }

    #[test]
    fn non_match_is_none() {
        assert!(window_around("nothing here", "needle", 20).is_none());
    }

    #[test]
    fn match_at_the_very_start_and_end_do_not_panic() {
        assert_eq!(window_around("needle", "needle", 10).unwrap(), "needle");
        // radius 1 cannot reach the leading "x", so the window is clipped and
        // says so rather than silently dropping text.
        assert_eq!(window_around("x needle", "needle", 1).unwrap(), "… needle");
        assert_eq!(window_around("x needle", "needle", 2).unwrap(), "x needle");
    }

    #[test]
    fn multibyte_text_is_sliced_on_char_boundaries() {
        // Chinese and emoji around the match: a byte-index slice would panic.
        let hay = format!("{}错误信息{}", "前".repeat(50), "🪶".repeat(50));
        let w = window_around(&hay, "错误信息", 5).unwrap();
        assert!(w.contains("错误信息"), "{w}");
    }

    #[test]
    fn newlines_are_collapsed_for_one_line_rows() {
        let w = window_around("line one\n\nline two NEEDLE here", "needle", 10).unwrap();
        assert!(!w.contains('\n'), "{w:?}");
    }
}
