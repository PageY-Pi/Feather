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

pub fn estimate_message_tokens(m: &crate::message::Message) -> u64 {
    estimate_message_tokens_filtered(m, true)
}

pub fn estimate_message_tokens_without_thinking(m:&crate::message::Message)->u64 {
    estimate_message_tokens_filtered(m, false)
}
fn estimate_message_tokens_filtered(m:&crate::message::Message, include_thinking:bool)->u64 {
    use crate::message::ContentBlock;
    let mut total = 4; // role overhead
    for b in &m.content {
        match b {
            ContentBlock::Text { text } => total += estimate_tokens(text) + 3,
            ContentBlock::Thinking { text, .. } if include_thinking => total += estimate_tokens(text) + 3,
            ContentBlock::Thinking { .. } => {},
            ContentBlock::ToolCall(c) => {
                // Estimate without re-serializing the arguments value — a full
                // serde pass per tool call per turn is pure waste for a rough
                // heuristic. Key names approximate the JSON payload size.
                let arguments = if let Some(s) = c.arguments.as_str() {
                    estimate_tokens(s)
                } else if let Some(object) = c.arguments.as_object() {
                    // Exactly the former joined-key estimate, without a Vec or
                    // String. Count whitespace boundaries across each key and
                    // the virtual separator inserted between consecutive keys.
                    let mut chars = 0u64;
                    let mut words = 0u64;
                    let mut in_word = false;
                    for (index, key) in object.keys().enumerate() {
                        if index > 0 { chars += 1; in_word = false; }
                        for ch in key.chars() {
                            chars += 1;
                            if ch.is_whitespace() { in_word = false; }
                            else if !in_word { words += 1; in_word = true; }
                        }
                    }
                    if chars == 0 { 0 } else { ((chars.div_ceil(4) * 3 + words) / 4).max(1) }
                } else { 0 };
                total += estimate_tokens(&c.name) + arguments + 8;
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

#[cfg(test)]
mod tests {
    #[test]
    fn argument_key_estimate_matches_previous_join_without_allocations() {
        use crate::message::{Message,ContentBlock,ToolCall,Api,Usage,StopReason};
        for arguments in [serde_json::json!({}),serde_json::json!({"":1," whitespace \t key":2,"羽毛":3}),serde_json::json!("words 羽毛"),serde_json::json!([1,2])] {
            let expected=match arguments.as_str(){Some(s)=>estimate_tokens(s),None=>arguments.as_object().map_or(0,|object|estimate_tokens(&object.keys().map(String::as_str).collect::<Vec<_>>().join(" ")))};
            let msg=Message::assistant(vec![ContentBlock::ToolCall(ToolCall{id:"t".into(),name:"read".into(),arguments}),ContentBlock::Text{text:"tail".into()}],"m",Api::Faux,Usage::default(),StopReason::ToolUse);
            assert_eq!(estimate_message_tokens(&msg),4+estimate_tokens("read")+expected+8+estimate_tokens("tail")+3);
        }
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
