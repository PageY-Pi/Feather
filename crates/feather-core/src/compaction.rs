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
    /// Input tokens left unused before the provider context limit triggers compaction.
    /// This is independent of the normal request's max output setting.
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
            keep_recent_tokens: 40000,
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

/// Decide from the complete provider prompt, rather than message rows alone.
/// System instructions and tool schemas count against the model context too.
/// `reserve_tokens` is compaction headroom; max output is deliberately not
/// subtracted because compaction runs asynchronously before the next request.
pub fn should_compact_request(
    request_input_tokens: u64,
    context_window: u64,
    reserve_tokens: u64,
) -> bool {
    request_input_tokens > context_window.saturating_sub(reserve_tokens)
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

/// Shrink arbitrary live-context text with a neutral marker. Raw audit text is
/// never passed here; this is only for the replaceable model context.
pub fn shrink_text(text: &str, budget: usize) -> (String, usize, usize) {
    let total = text.chars().count();
    if total <= budget || budget < 64 {
        return (text.to_string(), total, total);
    }
    let marker = "\n[feather: content elided by compaction]\n";
    let marker_len = marker.chars().count();
    let usable = budget.saturating_sub(marker_len);
    let head = usable * 3 / 5;
    let tail = usable.saturating_sub(head);
    let head_s: String = text.chars().take(head).collect();
    let tail_s: String = text.chars().skip(total.saturating_sub(tail)).collect();
    let removed = total.saturating_sub(head + tail);
    (format!("{head_s}{marker}{tail_s}"), total, removed)
}

/// Force a live context under an estimated-token budget. Complete oldest turns
/// are removed first; if the remaining turn is itself too large, text and tool
/// arguments are elided while preserving message roles and call/result pairing.
/// The durable raw audit is never passed here and therefore remains complete.
pub fn shrink_messages_to_tokens(messages: &mut Vec<Message>, budget: u64) {
    loop {
        if tokenizer::estimate_context_tokens(messages) <= budget {
            return;
        }
        // Keep the synthetic summary at index zero. Remove the oldest complete
        // kept turn whenever another user boundary exists — but never drop the
        // turn that holds the newest completed answer. A long tool-loop turn is
        // one user span, so draining to the *next* user message would delete
        // that entire piece of work (and the answer with it) instead of just
        // shrinking it.
        let protect_until = messages
            .iter()
            .rposition(|m| m.role == Role::Assistant && m.iter_tool_calls().next().is_none())
            .map(|answer| {
                messages[..=answer]
                    .iter()
                    .rposition(|m| m.role == Role::User)
                    .unwrap_or(0)
            })
            .unwrap_or(messages.len());
        let starts: Vec<usize> = messages
            .iter()
            .enumerate()
            .skip(1)
            .filter_map(|(i, m)| (m.role == Role::User).then_some(i))
            .collect();
        if starts.len() >= 2 && starts[1] <= protect_until {
            messages.drain(1..starts[1]);
            continue;
        }
        break;
    }

    for _ in 0..64 {
        let current = tokenizer::estimate_context_tokens(messages);
        if current <= budget {
            return;
        }
        let excess = current.saturating_sub(budget);
        let mut largest: Option<(usize, usize, usize)> = None;
        for (mi, message) in messages.iter().enumerate() {
            for (bi, block) in message.content.iter().enumerate() {
                let len = match block {
                    ContentBlock::Text { text } | ContentBlock::Thinking { text, .. } => {
                        text.chars().count()
                    }
                    ContentBlock::ToolCall(call) => call.arguments.to_string().chars().count(),
                    ContentBlock::Image { .. } => 0,
                };
                if largest.is_none_or(|(_, _, old)| len > old) {
                    largest = Some((mi, bi, len));
                }
            }
        }
        let Some((mi, bi, len)) = largest else { return };
        if len < 64 {
            return;
        }
        let target = len
            .saturating_sub((excess.max(1) * 4 + 64) as usize)
            .max(64);
        match &mut messages[mi].content[bi] {
            ContentBlock::Text { text } | ContentBlock::Thinking { text, .. } => {
                let (shrunk, _, _) = shrink_text(text, target);
                *text = shrunk;
            }
            ContentBlock::ToolCall(call) => {
                call.arguments = serde_json::json!({
                    "_feather_arguments_elided": true,
                    "tool": call.name,
                });
            }
            ContentBlock::Image { .. } => return,
        }
    }
}
/// Serialize messages for the summarizer with tool results shrunk to
/// `budget` chars (pi-style `[User]:` / `[Assistant]:` framing so the model
/// treats it as material, not a conversation to continue).
pub fn serialize_conversation(messages: &[Message], budget: usize) -> String {
    serialize_messages(messages.iter(), budget)
}

/// Borrowed serializer shared by callers that filter without cloning messages.
pub fn serialize_messages<'a>(
    messages: impl IntoIterator<Item = &'a Message>,
    budget: usize,
) -> String {
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
pub fn without_thinking(messages: &[Message]) -> Vec<Message> {
    messages
        .iter()
        .map(|m| Message {
            role: m.role,
            content: m
                .content
                .iter()
                .filter(|b| !matches!(b, ContentBlock::Thinking { .. }))
                .cloned()
                .collect(),
            tool_call_id: m.tool_call_id.clone(),
            tool_name: m.tool_name.clone(),
            is_error: m.is_error,
            model: m.model.clone(),
            api: m.api,
            usage: m.usage,
            stop_reason: m.stop_reason,
            timestamp: m.timestamp,
            shrink: m.shrink.clone(),
        })
        .collect()
}
/// Find the cut point: the index of the first message of the kept span.
///
/// Walks backwards accumulating token estimates; when the budget is hit the
/// cut snaps forward to the next turn boundary (start of a user-message
/// span) so tool results are never separated from their calls, and whole
/// turns stay intact (线性上下文 turn 粒度).
pub fn plan_cut(messages: &[Message], keep_recent_tokens: u64) -> usize {
    plan_cut_with(
        messages,
        keep_recent_tokens,
        tokenizer::estimate_message_tokens,
        true,
    )
}
pub fn plan_cut_without_thinking(messages: &[Message], keep_recent_tokens: u64) -> usize {
    plan_cut_with(
        messages,
        keep_recent_tokens,
        tokenizer::estimate_message_tokens_without_thinking,
        true,
    )
}
/// Budget-driven cut that never protects a whole oversized turn.
///
/// [`plan_cut_without_thinking`] keeps the newest completed answer's entire
/// user span intact. That is the right default, but one prompt that runs a
/// very long tool loop makes that span most of the context: the kept span then
/// eats the whole window, summarization saves almost nothing, and the next
/// turn is over the trigger again — compaction on every single turn. Callers
/// that can measure the remaining room use this variant when protection would
/// starve the working context.
pub fn plan_cut_budget(messages: &[Message], keep_recent_tokens: u64) -> usize {
    plan_cut_with(
        messages,
        keep_recent_tokens,
        tokenizer::estimate_message_tokens_without_thinking,
        false,
    )
}
fn plan_cut_with(
    messages: &[Message],
    keep_recent_tokens: u64,
    estimate: fn(&Message) -> u64,
    protect_answer_span: bool,
) -> usize {
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
        // No user boundary inside the budget: the newest turn on its own is
        // bigger than `keep_recent_tokens` (one prompt that ran a long tool
        // loop). Keeping that turn whole is only affordable while the caller
        // still has room to spare; otherwise the kept span swallows the window
        // and compaction re-fires on every turn.
        cut = if protect_answer_span {
            // Display-first default: keep the whole turn containing the newest
            // completed answer. Callers that can measure the remaining room
            // re-cut with [`plan_cut_budget`] when that costs too much.
            let mut start = idx;
            while start > 0 && messages[start].role != Role::User {
                start -= 1;
            }
            start
        } else {
            // Nearest safe boundary at or after the budget point. An assistant
            // message never orphans a tool result: call/result pairs stay whole
            // inside the kept span and the summarized prefix still ends on a
            // complete pair. A user message here is the newly appended prompt;
            // the answer clamp below keeps the work that prompt is about.
            (idx..messages.len())
                .find(|&i| matches!(messages[i].role, Role::User | Role::Assistant))
                .unwrap_or(idx)
        };
    }
    // A newly appended user prompt can consume the entire recent-token budget.
    // Do not summarize away the latest completed answer (and its whole user/tool
    // span), which callers still need to display/reopen while that prompt runs.
    // The clamp must never collapse the cut to zero, though: on tool-loop
    // sessions the only plain answer can sit in the first span, and a zero cut
    // makes every compaction a permanent no-op — the caller then aborts the
    // run as over-limit. When the protected span is the first one, keep the
    // budget-driven cut: a working context beats a protected display, and the
    // raw audit still preserves the answer verbatim.
    if let Some(last_answer) = messages
        .iter()
        .rposition(|m| m.role == Role::Assistant && m.iter_tool_calls().next().is_none())
    {
        if protect_answer_span {
            let span_start = messages[..=last_answer]
                .iter()
                .rposition(|m| m.role == Role::User)
                .unwrap_or(0);
            let clamped = cut.min(span_start);
            if clamped > 0 {
                cut = clamped;
            }
        } else if cut > last_answer {
            // Even an oversized turn keeps its answer: the tool work behind it
            // is summarized, the text the caller shows is not.
            cut = last_answer;
        }
    }
    cut
}

/// Actual character count of model-context payloads (not UTF-8 bytes). Images
/// are not shortened by compaction and are excluded from character savings.
pub fn context_characters(messages: &[Message]) -> usize {
    messages
        .iter()
        .flat_map(|m| &m.content)
        .map(|block| match block {
            ContentBlock::Text { text } | ContentBlock::Thinking { text, .. } => {
                text.chars().count()
            }
            ContentBlock::ToolCall(call) => {
                call.name.chars().count() + call.arguments.to_string().chars().count()
            }
            ContentBlock::Image { .. } => 0,
        })
        .sum()
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
        let mut messages = Vec::new();
        for n in 0..12 {
            messages.push(Message::user_text(format!("request {n}")));
            messages.push(Message::assistant(
                vec![
                    ContentBlock::Thinking {
                        text: "private reasoning".repeat(1000),
                        signature: None,
                    },
                    ContentBlock::Text {
                        text: "answer".into(),
                    },
                ],
                "m",
                crate::message::Api::Faux,
                crate::message::Usage::default(),
                crate::message::StopReason::Stop,
            ));
        }
        let clean = without_thinking(&messages);
        for budget in [0, 10, 100, 10000] {
            assert_eq!(
                plan_cut_without_thinking(&messages, budget),
                plan_cut(&clean, budget)
            );
        }
        assert_eq!(
            serialize_messages(messages.iter(), 2000),
            serialize_conversation(&clean, 2000)
        );
    }

    #[test]
    fn summary_and_kept_context_exclude_reasoning_but_raw_is_unchanged() {
        let mut answer = Message::assistant(
            Vec::new(),
            "synthetic",
            crate::message::Api::Faux,
            crate::message::Usage::default(),
            crate::message::StopReason::Stop,
        );
        answer.content.push(ContentBlock::Thinking {
            text: "PRIVATE MODEL REASONING".into(),
            signature: None,
        });
        answer.content.push(ContentBlock::Text {
            text: "final answer".into(),
        });
        let raw = vec![Message::user_text("request"), answer];
        let input = serialize_conversation(&raw, 2000);
        assert!(!input.contains("PRIVATE MODEL REASONING"));
        assert!(input.contains("final answer"));
        let kept = without_thinking(&raw);
        assert!(kept[1].thinking().is_empty());
        assert_eq!(kept[1].text(), "final answer");
        assert_eq!(raw[1].thinking(), "PRIVATE MODEL REASONING");
        assert_eq!(kept[1].timestamp, raw[1].timestamp);
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
    fn defaults_match_pi_preserved_span_and_headroom() {
        let settings = CompactionSettings::default();
        assert_eq!(settings.reserve_tokens, 16_384);
        assert_eq!(settings.keep_recent_tokens, 40_000);
        assert!(!should_compact(255_616, 272_000, &settings));
        assert!(should_compact(255_617, 272_000, &settings));
    }

    #[test]
    fn request_threshold_does_not_reserve_max_output() {
        assert!(should_compact_request(255_617, 272_000, 16_384));
        assert!(!should_compact_request(255_616, 272_000, 16_384));
        assert!(!should_compact_request(271_999, 272_000, 0));
        assert!(should_compact_request(272_001, 272_000, 0));
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
