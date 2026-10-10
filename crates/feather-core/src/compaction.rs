//! Compaction with tool-result shrinking (compaction 时缩 tool result).
//!
//! Two shrinking sites, one feature:
//! 1. **Summary input**: the conversation serialized for the summarizer
//!    truncates every tool result to `summary_tool_result_budget` chars
//!    (pi parity: 2000).
//! 2. **Kept context**: tool results that survive compaction but are older
//!    than the last `keep_last_turns_unshrunk` turns are rewritten in place
//!    to head+tail around an elision marker (`shrink_kept = true`). The raw
//!    transcript keeps the original; the shrink is recorded on the message
//!    (`ShrinkInfo`) so later rebuilds stay byte-stable — which keeps the
//!    prompt-cache prefix stable after the one-time cost.

use crate::message::{ContentBlock, Message, Role};
use crate::tokenizer;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CompactionSettings {
    pub enabled: bool,
    /// Tokens reserved for the model's response.
    pub reserve_tokens: u64,
    /// Recent tokens that survive compaction un-summarized.
    pub keep_recent_tokens: u64,
    /// Max chars of each tool result inside the summarizer input.
    pub summary_tool_result_budget: usize,
    /// Max chars of a tool result kept in live context after shrinking.
    pub kept_tool_result_budget: usize,
    /// Shrink oversized tool results in the kept span (feature core).
    pub shrink_kept: bool,
    /// Newest N turns never touched by kept-span shrinking.
    pub keep_last_turns_unshrunk: u64,
}

impl Default for CompactionSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            reserve_tokens: 16384,
            keep_recent_tokens: 20000,
            summary_tool_result_budget: 2000,
            kept_tool_result_budget: 4000,
            shrink_kept: true,
            keep_last_turns_unshrunk: 2,
        }
    }
}

pub const SUMMARY_SYSTEM: &str = "You are a conversation summarizer for a coding-agent session. \
Produce a compact, factual summary in markdown using exactly these sections: \
## Goal, ## Constraints & Preferences, ## Progress (### Done / ### In Progress / ### Blocked), \
## Key Decisions, ## Next Steps, ## Critical Context. \
Preserve exact file paths, commands and identifiers. Do not invent content.";

pub fn should_compact(
    context_tokens: u64,
    context_window: u64,
    settings: &CompactionSettings,
) -> bool {
    settings.enabled && context_tokens > context_window.saturating_sub(settings.reserve_tokens)
}

/// Head+tail shrink around an elision marker. Never returns something longer
/// than the input; the marker states how much was removed.
pub fn shrink_tool_result(text: &str, budget: usize) -> (String, usize, usize) {
    let total = text.chars().count();
    if total <= budget || budget < 64 {
        return (text.to_string(), total, total);
    }
    let head = budget * 3 / 5;
    let tail = budget - head;
    let head_s: String = text.chars().take(head).collect();
    let tail_s: String = text.chars().skip(total - tail).collect();
    let elided = total - head - tail;
    let out = format!("{head_s}\n[feather: {elided} chars elided by compaction]\n{tail_s}");
    (out, total, elided)
}

/// Serialize messages for the summarizer with tool results shrunk to
/// `budget` chars (pi-style `[User]:` / `[Assistant]:` framing so the model
/// treats it as material, not a conversation to continue).
pub fn serialize_conversation(messages: &[Message], budget: usize) -> String {
    serialize_messages(messages.iter(), budget)
}

/// Borrowed serializer shared by callers that filter without cloning messages.
pub fn serialize_messages<'a>(messages: impl IntoIterator<Item = &'a Message>, budget: usize) -> String {
    let mut out = String::new();
    for m in messages {
        match m.role {
            Role::User => {
                out.push_str("[User]: ");
                out.push_str(&m.text_cow());
                out.push('\n');
            }
            Role::Assistant => {
                // Reasoning is private model output, not summary source material.
                // Keep raw audit thinking for GUI inspection, never feed it to compaction.
                let mut calls = m.iter_tool_calls().peekable();
                if calls.peek().is_some() {
                    out.push_str("[Assistant tool calls]: ");
                    for (i, c) in calls.enumerate() {
                        if i > 0 {
                            out.push_str("; ");
                        }
                        out.push_str(&c.name);
                        out.push('(');
                        out.push_str(&c.arguments.to_string());
                        out.push(')');
                    }
                    out.push('\n');
                }
                let t = m.text_cow();
                if !t.is_empty() {
                    out.push_str("[Assistant]: ");
                    out.push_str(&t);
                    out.push('\n');
                }
            }
            Role::ToolResult => {
                let (shrunk, _, _) = shrink_tool_result(&m.text_cow(), budget);
                out.push_str("[Tool result]: ");
                out.push_str(&shrunk);
                out.push('\n');
            }
            Role::System => {}
        }
    }
    out
}

/// Strip model reasoning from compacted model context only. Never mutate the
/// durable raw audit messages, which power visible thinking cards and rewind.
pub fn without_thinking(messages:&[Message])->Vec<Message> {
    messages.iter().map(|m|Message {
        role: m.role,
        content: m.content.iter().filter(|b|!matches!(b,ContentBlock::Thinking{..})).cloned().collect(),
        tool_call_id: m.tool_call_id.clone(), tool_name: m.tool_name.clone(),
        is_error: m.is_error, model: m.model.clone(), api: m.api,
        usage: m.usage, stop_reason: m.stop_reason, timestamp: m.timestamp,
        shrink: m.shrink.clone(),
    }).collect()
}
/// Find the cut point: the index of the first message of the kept span.
///
/// Walks backwards accumulating token estimates; when the budget is hit the
/// cut snaps forward to the next turn boundary (start of a user-message
/// span) so tool results are never separated from their calls, and whole
/// turns stay intact (线性上下文 turn 粒度).
pub fn plan_cut(messages: &[Message], keep_recent_tokens: u64) -> usize {
    plan_cut_with(messages, keep_recent_tokens, tokenizer::estimate_message_tokens)
}
pub fn plan_cut_without_thinking(messages:&[Message],keep_recent_tokens:u64)->usize {
    plan_cut_with(messages,keep_recent_tokens,tokenizer::estimate_message_tokens_without_thinking)
}
fn plan_cut_with(messages:&[Message],keep_recent_tokens:u64,estimate:fn(&Message)->u64)->usize {
    if messages.is_empty() {
        return 0;
    }
    let mut acc = 0u64;
    let mut idx = messages.len();
    while idx > 0 {
        idx -= 1;
        acc += estimate(&messages[idx]);
        if acc >= keep_recent_tokens {
            break;
        }
    }
    // snap forward to the next turn boundary (a User message at or after idx)
    let mut cut = idx;
    while cut < messages.len() && messages[cut].role != Role::User {
        cut += 1;
    }
    if cut >= messages.len() {
        // no user boundary inside the budget (one user span exceeds it):
        // walk back to the previous user-span start instead — the only cut
        // that never separates an assistant's tool call from its result.
        cut = idx;
        while cut > 0 && messages[cut].role != Role::User {
            cut -= 1;
        }
    }
    // A newly appended user prompt can consume the entire recent-token budget.
    // Do not summarize away the latest completed answer (and its whole user/tool
    // span), which callers still need to display/reopen while that prompt runs.
    if let Some(last_answer) = messages.iter().rposition(|m| {
        m.role == Role::Assistant && m.iter_tool_calls().next().is_none()
    }) {
        let span_start = messages[..=last_answer].iter()
            .rposition(|m| m.role == Role::User).unwrap_or(0);
        cut = cut.min(span_start);
    }
    cut
}

/// Actual character count of model-context payloads (not UTF-8 bytes). Images
/// are not shortened by compaction and are excluded from character savings.
pub fn context_characters(messages: &[Message]) -> usize {
    messages.iter().flat_map(|m| &m.content).map(|block| match block {
        ContentBlock::Text { text } | ContentBlock::Thinking { text, .. } => text.chars().count(),
        ContentBlock::ToolCall(call) => call.name.chars().count() + call.arguments.to_string().chars().count(),
        ContentBlock::Image { .. } => 0,
    }).sum()
}
/// Apply kept-span shrinking in place: tool results outside the newest
/// `keep_last_turns_unshrunk` turns and over budget are replaced by their
/// shrunk form. Returns the number of chars saved.
pub fn shrink_kept_span(messages: &mut [Message], settings: &CompactionSettings) -> usize {
    if !settings.shrink_kept {
        return 0;
    }
    // identify turn starts (user messages) to count turns from the end
    let n = messages.len();
    let mut turn_of = vec![0u64; n];
    let mut t = 0u64;
    let mut saw_user = false;
    for i in 0..n {
        if messages[i].role == Role::User {
            t += u64::from(saw_user); // first user message belongs to turn 0
            saw_user = true;
        }
        turn_of[i] = t;
    }
    let last_turn = t;
    let mut saved = 0usize;
    for i in 0..n {
        if messages[i].role != Role::ToolResult {
            continue;
        }
        if last_turn.saturating_sub(turn_of[i]) < settings.keep_last_turns_unshrunk {
            continue;
        }
        if messages[i].shrink.is_some() {
            continue; // already shrunk; keep byte-stable
        }
        let text = messages[i].text();
        let (shrunk, original_len, _) = shrink_tool_result(&text, settings.kept_tool_result_budget);
        if shrunk.len() < text.len() {
            saved += text.len() - shrunk.len();
            messages[i].content = vec![ContentBlock::Text { text: shrunk }];
            messages[i].shrink = Some(crate::message::ShrinkInfo {
                original_len,
                head: settings.kept_tool_result_budget * 3 / 5,
                tail: settings.kept_tool_result_budget * 2 / 5,
            });
        }
    }
    saved
}

/// Extract cumulative read/modified file lists from tool calls (pi-parity
/// `<read-files>` / `<modified-files>` blocks).
pub fn file_ops(messages: &[Message]) -> (Vec<String>, Vec<String>) {
    let mut reads = Vec::new();
    let mut writes = Vec::new();
    // membership sets keep the dedup O(1); the vecs preserve first-seen order
    let mut seen_reads = std::collections::HashSet::new();
    let mut seen_writes = std::collections::HashSet::new();
    let push = |v: &mut Vec<String>, seen: &mut std::collections::HashSet<String>, s: String| {
        if seen.insert(s.clone()) {
            v.push(s);
        }
    };
    for m in messages {
        for c in m.iter_tool_calls() {
            let path = c
                .arguments
                .get("path")
                .or_else(|| c.arguments.get("file_path"))
                .and_then(|p| p.as_str())
                .unwrap_or("");
            if path.is_empty() {
                continue;
            }
            match c.name.as_str() {
                "read" | "anchor_grep" | "grep" => {
                    push(&mut reads, &mut seen_reads, path.to_string());
                }
                "write" | "replace" | "replace_within" | "insert" | "copy" | "move" => {
                    push(&mut reads, &mut seen_reads, path.to_string());
                    push(&mut writes, &mut seen_writes, path.to_string());
                }
                _ => {}
            }
        }
    }
    (reads, writes)
}

/// Build the user message carrying the summary into live context.
pub fn summary_message(summary: &str, reads: &[String], writes: &[String]) -> Message {
    let mut text = String::from("## Conversation summary (feather compaction)\n\n");
    text.push_str(summary);
    if !reads.is_empty() {
        text.push_str("\n\n<read-files>\n");
        for r in reads {
            text.push_str(r);
            text.push('\n');
        }
        text.push_str("</read-files>");
    }
    if !writes.is_empty() {
        text.push_str("\n\n<modified-files>\n");
        for w in writes {
            text.push_str(w);
            text.push('\n');
        }
        text.push_str("</modified-files>");
    }
    Message::user_text(text)
}

#[cfg(test)]
mod tests {
    #[test]
    fn borrowed_cut_without_reasoning_matches_sanitized_cut() {
        let mut messages=Vec::new();
        for n in 0..12 {
            messages.push(Message::user_text(format!("request {n}")));
            messages.push(Message::assistant(vec![ContentBlock::Thinking{text:"private reasoning".repeat(1000),signature:None},ContentBlock::Text{text:"answer".into()}],"m",crate::message::Api::Faux,crate::message::Usage::default(),crate::message::StopReason::Stop));
        }
        let clean=without_thinking(&messages);
        for budget in [0,10,100,10000] {assert_eq!(plan_cut_without_thinking(&messages,budget),plan_cut(&clean,budget));}
        assert_eq!(serialize_messages(messages.iter(),2000),serialize_conversation(&clean,2000));
    }

    #[test]
    fn summary_and_kept_context_exclude_reasoning_but_raw_is_unchanged(){
        let mut answer=Message::assistant(Vec::new(),"synthetic",crate::message::Api::Faux,crate::message::Usage::default(),crate::message::StopReason::Stop);answer.content.push(ContentBlock::Thinking{text:"PRIVATE MODEL REASONING".into(),signature:None});answer.content.push(ContentBlock::Text{text:"final answer".into()});
        let raw=vec![Message::user_text("request"),answer];let input=serialize_conversation(&raw,2000);
        assert!(!input.contains("PRIVATE MODEL REASONING"));assert!(input.contains("final answer"));
        let kept=without_thinking(&raw);assert!(kept[1].thinking().is_empty());assert_eq!(kept[1].text(),"final answer");assert_eq!(raw[1].thinking(),"PRIVATE MODEL REASONING");assert_eq!(kept[1].timestamp,raw[1].timestamp);
    }
    use super::*;
    use crate::message::{Api, StopReason, Usage};

    fn asst(text: &str) -> Message {
        Message::assistant(
            vec![ContentBlock::Text { text: text.into() }],
            "m",
            Api::Faux,
            Usage::default(),
            StopReason::Stop,
        )
    }

    #[test]
    fn shrink_respects_budget_and_marker() {
        let long = "x".repeat(10_000);
        let (out, total, elided) = shrink_tool_result(&long, 1000);
        assert_eq!(total, 10_000);
        assert_eq!(elided, 9_000);
        assert!(out.contains("[feather: 9000 chars elided by compaction]"));
        assert!(out.len() < 1200);
        // short text untouched
        let (out2, t2, e2) = shrink_tool_result("short", 1000);
        assert_eq!(out2, "short");
        assert_eq!((t2, e2), (5, 5));
    }

    #[test]
    fn serialization_truncates_tool_results() {
        let msgs = vec![
            Message::user_text("run it"),
            asst(""),
            Message::tool_result("1", "bash", "y".repeat(5000), false),
        ];
        let s = serialize_conversation(&msgs, 2000);
        assert!(s.starts_with("[User]: run it"));
        assert!(s.contains("[Tool result]: "));
        assert!(s.len() < 2400);
    }

    #[test]
    fn cut_snaps_to_user_boundary() {
        let msgs = vec![
            Message::user_text("t1"),                      // 0
            asst("a"),                                     // 1
            Message::tool_result("1", "bash", "r", false), // 2
            Message::user_text("t2"),                      // 3
            asst("b"),                                     // 4
            Message::tool_result("2", "bash", "r", false), // 5
        ];
        // tiny budget: cut must land on a User message
        let cut = plan_cut(&msgs, 1);
        assert_eq!(msgs[cut].role, Role::User);
        assert_eq!(cut, 3);
        // generous budget: keep everything
        assert_eq!(plan_cut(&msgs, 1_000_000), 0);
    }

    #[test]
    fn kept_span_shrinks_old_results_only() {
        let mut msgs = vec![];
        for i in 0..5 {
            msgs.push(Message::user_text(format!("turn {i}")));
            msgs.push(asst(""));
            msgs.push(Message::tool_result(
                format!("{i}"),
                "read",
                "z".repeat(10_000),
                false,
            ));
        }
        let saved = shrink_kept_span(&mut msgs, &CompactionSettings::default());
        assert!(saved > 0);
        // last two turns untouched
        let last = msgs.last().unwrap();
        assert_eq!(last.text().len(), 10_000);
        assert!(last.shrink.is_none());
        // older ones shrunk
        let first = msgs[2].clone();
        assert_eq!(first.role, Role::ToolResult);
        assert!(first.shrink.is_some());
        assert!(first.text().len() < 5_000);
        // idempotent
        let saved2 = shrink_kept_span(&mut msgs, &CompactionSettings::default());
        assert_eq!(saved2, 0);
    }

    #[test]
    fn threshold_math() {
        let s = CompactionSettings::default();
        assert!(!should_compact(100_000, 200_000, &s));
        assert!(should_compact(190_000, 200_000, &s));
        let mut off = s.clone();
        off.enabled = false;
        assert!(!should_compact(1_000_000, 200_000, &off));
    }
}
