//! Heuristic token estimator.
//!
//! feather intentionally ships no BPE tables. Estimates are used for the
//! compaction threshold and budget arithmetic; exact usage always comes from
//! the provider response and is recorded per assistant message.

/// Words-in-text heuristic: ~4 chars per token, word-boundary aware, with a
/// small surcharge for JSON punctuation density. Deterministic and fast.
pub fn estimate_tokens(text: &str) -> u64 {
    if text.is_empty() {
        return 0;
    }
    let chars = text.chars().count() as u64;
    let words = text.split_whitespace().count() as u64;
    // blend: char/4 floor plus a word-based nudge, capped sensibly
    let by_char = chars.div_ceil(4);
    let est = (by_char * 3 + words) / 4;
    est.max(1)
}

/// Token estimate for a JSON value as it appears inside a request body.
///
/// Walks the value instead of serializing it: the shape is all that matters for
/// a heuristic, and a full `to_string()` per tool call per turn would allocate
/// megabytes on a session that writes large files. Values are counted — that is
/// the whole point, since a tool call's arguments are sent verbatim.
fn estimate_json_tokens(value: &serde_json::Value) -> u64 {
    use serde_json::Value;
    match value {
        Value::Null => 4,
        Value::Bool(true) => 4,
        Value::Bool(false) => 5,
        Value::Number(n) => n.to_string().len() as u64,
        // Quotes are two characters; escape sequences are ignored, which can
        // only under-count a pathological string.
        Value::String(s) => s.chars().count() as u64 + 2,
        Value::Array(items) => {
            items.iter().map(estimate_json_tokens).sum::<u64>() + 2 + items.len() as u64
        }
        Value::Object(entries) => {
            entries
                .iter()
                .map(|(key, item)| {
                    estimate_json_tokens(&Value::String(key.clone())) + estimate_json_tokens(item)
                })
                .sum::<u64>()
                + 2
                + entries.len() as u64
        }
    }
}

pub fn estimate_message_tokens(m: &crate::message::Message) -> u64 {
    estimate_message_tokens_filtered(m, true)
}

pub fn estimate_message_tokens_without_thinking(m: &crate::message::Message) -> u64 {
    estimate_message_tokens_filtered(m, false)
}
fn estimate_message_tokens_filtered(m: &crate::message::Message, include_thinking: bool) -> u64 {
    use crate::message::ContentBlock;
    let mut total = 4; // role overhead
    for b in &m.content {
        match b {
            ContentBlock::Text { text } => total += estimate_tokens(text) + 3,
            ContentBlock::Thinking { text, .. } if include_thinking => {
                total += estimate_tokens(text) + 3
            }
            ContentBlock::Thinking { .. } => {}
            ContentBlock::ToolCall(c) => {
                // Arguments are sent verbatim as JSON, so values must count: a
                // `write` call carrying a large file body is the biggest thing a
                // request can contain. Measured without allocating the whole
                // serialized form on every turn.
                total += estimate_tokens(&c.name) + estimate_json_tokens(&c.arguments) + 8;
            }
            // images: rough fixed cost per block (real cost depends on the
            // provider's vision pricing; keep it conservative)
            ContentBlock::Image { .. } => total += 800,
        }
    }
    total
}

/// Token count of the whole linear context (what would be sent).
pub fn estimate_context_tokens(msgs: &[crate::message::Message]) -> u64 {
    msgs.iter().map(estimate_message_tokens).sum::<u64>() + 16
}

/// Estimate the tokens occupied by a tool schema in the provider request.
/// This is deliberately conservative: the wire providers add role/function
/// wrappers around the same name, description, and JSON schema.
pub fn estimate_tool_tokens(tool: &crate::provider::ToolDef) -> u64 {
    12 + estimate_tokens(&tool.name)
        + estimate_tokens(&tool.description)
        + serde_json::to_string(&tool.parameters)
            .map(|json| estimate_tokens(&json))
            .unwrap_or(0)
}

/// Estimate the complete prompt-side request, including system instructions
/// and tool definitions. `Session::context_tokens` intentionally tracks only
/// messages for cache metrics; compaction must use this larger number because
/// providers enforce their context limit on the complete request.
///
/// The estimate counts reasoning only when `api` actually replays it — see
/// [`crate::message::Api::sends_reasoning_in_history`]. Providers that drop
/// reasoning from the request body would otherwise appear to hold far more
/// context than they send, which triggers compaction that has nothing to
/// summarize and can fail the turn for a request the endpoint would accept.
pub fn estimate_request_input_tokens(
    api: crate::message::Api,
    system: &str,
    messages: &[crate::message::Message],
    tools: &[crate::provider::ToolDef],
) -> u64 {
    let system_tokens = if system.is_empty() {
        0
    } else {
        estimate_tokens(system) + 4
    };
    let message_tokens = if api.sends_reasoning_in_history() {
        messages.iter().map(estimate_message_tokens).sum::<u64>()
    } else {
        messages
            .iter()
            .map(estimate_message_tokens_without_thinking)
            .sum::<u64>()
    };
    system_tokens + message_tokens + tools.iter().map(estimate_tool_tokens).sum::<u64>() + 16
}

#[cfg(test)]
mod tests {
    #[test]
    fn tool_arguments_are_estimated_from_their_values() {
        use crate::message::{Api, ContentBlock, Message, StopReason, ToolCall, Usage};
        let message = |arguments: serde_json::Value| {
            Message::assistant(
                vec![
                    ContentBlock::ToolCall(ToolCall {
                        id: "t".into(),
                        name: "read".into(),
                        arguments,
                    }),
                    ContentBlock::Text {
                        text: "tail".into(),
                    },
                ],
                "m",
                Api::Faux,
                Usage::default(),
                StopReason::ToolUse,
            )
        };
        // Values are sent verbatim, so a large payload must dominate the count.
        let small = estimate_message_tokens(&message(serde_json::json!({"path": "a"})));
        let large =
            estimate_message_tokens(&message(serde_json::json!({"path": "a".repeat(200_000)})));
        assert!(
            large > small + 40_000,
            "a 200 KB argument must count: small={small} large={large}"
        );

        // The exact structure is not the point; the values are. Pin the shape of
        // the walk so a future change cannot silently drop them again.
        assert_eq!(estimate_json_tokens(&serde_json::json!("abcd")), 6);
        // 2 braces + 1 comma + ("k" quoted) 3 + ("abcd" quoted) 6
        assert_eq!(estimate_json_tokens(&serde_json::json!({"k": "abcd"})), 12);
        assert_eq!(estimate_json_tokens(&serde_json::json!([1, 22])), 7);
    }
    use super::*;

    #[test]
    fn basic_estimates() {
        assert_eq!(estimate_tokens(""), 0);
        assert!(estimate_tokens("hello world") >= 2);
        assert!(estimate_tokens("hello world") < 10);
        let long = "word ".repeat(1000);
        assert!(estimate_tokens(&long) > 1000);
    }
}
