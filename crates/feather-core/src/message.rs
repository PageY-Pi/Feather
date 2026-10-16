//! Message and content-block types for the feather kernel.
//!
//! Mirrors the pi message model (see pi docs/message-types.md) but reduced to
//! what a linear, turn-granularity session needs: `system`, `user`,
//! `assistant`, `tool_result`. No extension/custom roles — feather has no
//! extensions.

use serde::{Deserialize, Serialize};
use std::fmt;

/// Provider-agnostic role.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    System,
    User,
    Assistant,
    ToolResult,
}

/// A tool call emitted by an assistant message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    /// Raw JSON arguments object.
    pub arguments: serde_json::Value,
}

/// Text or other typed content inside a message body.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    Thinking {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
    },
    ToolCall(ToolCall),
    /// Base64 image block (pi's attachment pipeline: @file mentions of
    /// image paths become image blocks, carried provider-natively).
    Image {
        media_type: String,
        data: String,
    },
}

/// Token accounting returned by a provider for one request.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
}

impl Usage {
    pub fn total(&self) -> u64 {
        self.input + self.output + self.cache_read + self.cache_write
    }
}

/// Why an assistant response stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    Stop,
    ToolUse,
    Length,
    Aborted,
    Error,
}

/// Which provider/api produced an assistant message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Api {
    AnthropicMessages,
    OpenaiCompletions,
    OpenaiResponses,
    OpenaiCodexResponses,
    PiMessages,
    Faux,
}

impl Api {
    /// Whether stored reasoning is replayed in the request history.
    ///
    /// Only some providers accept reasoning back: anthropic replays signed
    /// thinking blocks, `openai-responses` replays encrypted reasoning, and
    /// `pi-messages` sends `thinking` parts. `openai-completions` — used by
    /// volcengine-plan, deepseek, Bailian and the rest of the compatible
    /// endpoints — serializes `Message::text()` plus tool calls only, so
    /// reasoning never reaches the wire.
    ///
    /// The distinction decides whether reasoning may be counted against the
    /// model's context window: counting what is never sent makes a session look
    /// oversized, triggers pointless compaction, and can fail the turn outright.
    pub fn sends_reasoning_in_history(self) -> bool {
        !matches!(self, Self::OpenaiCompletions | Self::Faux)
    }
}

/// A single message in the linear session history.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: Vec<ContentBlock>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(default)]
    pub is_error: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api: Option<Api>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<StopReason>,
    /// Unix millis.
    pub timestamp: u64,
    /// Set when compaction shrunk this tool result; original lives only in
    /// the transcript file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shrink: Option<ShrinkInfo>,
}

/// Record of a tool result that compaction truncated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShrinkInfo {
    pub original_len: usize,
    pub head: usize,
    pub tail: usize,
}

impl Message {
    pub fn user_text(text: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: vec![ContentBlock::Text { text: text.into() }],
            tool_call_id: None,
            tool_name: None,
            is_error: false,
            model: None,
            api: None,
            usage: None,
            stop_reason: None,
            timestamp: now_millis(),
            shrink: None,
        }
    }

    pub fn assistant(
        content: Vec<ContentBlock>,
        model: impl Into<String>,
        api: Api,
        usage: Usage,
        stop_reason: StopReason,
    ) -> Self {
        Self {
            role: Role::Assistant,
            content,
            tool_call_id: None,
            tool_name: None,
            is_error: false,
            model: Some(model.into()),
            api: Some(api),
            usage: Some(usage),
            stop_reason: Some(stop_reason),
            timestamp: now_millis(),
            shrink: None,
        }
    }

    pub fn tool_result(
        tool_call_id: impl Into<String>,
        tool_name: impl Into<String>,
        text: impl Into<String>,
        is_error: bool,
    ) -> Self {
        Self {
            role: Role::ToolResult,
            content: vec![ContentBlock::Text { text: text.into() }],
            tool_call_id: Some(tool_call_id.into()),
            tool_name: Some(tool_name.into()),
            is_error,
            model: None,
            api: None,
            usage: None,
            stop_reason: None,
            timestamp: now_millis(),
            shrink: None,
        }
    }

    /// The usual single text block can be borrowed. Multiple blocks keep the
    /// legacy newline joining semantics; only that case needs an allocation.
    pub fn text_cow(&self) -> std::borrow::Cow<'_, str> {
        let mut blocks = self.content.iter().filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        });
        let Some(first) = blocks.next() else {
            return std::borrow::Cow::Borrowed("");
        };
        if blocks.next().is_none() {
            std::borrow::Cow::Borrowed(first)
        } else {
            std::borrow::Cow::Owned(self.text())
        }
    }
    pub fn text(&self) -> String {
        let mut out = String::new();
        for b in &self.content {
            if let ContentBlock::Text { text } = b {
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(text);
            }
        }
        out
    }

    pub fn iter_tool_calls(&self) -> impl Iterator<Item = &ToolCall> {
        self.content.iter().filter_map(|block| match block {
            ContentBlock::ToolCall(call) => Some(call),
            _ => None,
        })
    }
    pub fn tool_calls(&self) -> Vec<&ToolCall> {
        self.content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::ToolCall(c) => Some(c),
                _ => None,
            })
            .collect()
    }

    pub fn thinking(&self) -> String {
        let mut out = String::new();
        for b in &self.content {
            if let ContentBlock::Thinking { text, .. } = b {
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(text);
            }
        }
        out
    }

    pub fn role_name(&self) -> &'static str {
        match self.role {
            Role::System => "system",
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::ToolResult => "tool_result",
        }
    }
}

impl fmt::Display for Message {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}]", self.role_name())?;
        match self.role {
            Role::Assistant => {
                let t = self.thinking();
                if !t.is_empty() {
                    write!(f, " thinking: {t} ")?;
                }
                for c in self.tool_calls() {
                    write!(f, " {}({})", c.name, c.arguments)?;
                }
                let t = self.text();
                if !t.is_empty() {
                    write!(f, " {t}")?;
                }
                Ok(())
            }
            _ => write!(f, " {}", self.text()),
        }
    }
}

pub fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// FNV-1a 64-bit — stable across runs, used for cache-prefix comparison and
/// snapshot chunking (not cryptographic).
pub fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// Hash canonical JSON directly into FNV rather than allocating a temporary
/// serialized copy. Used only for stable cache generations, not cryptography.
pub fn json_hash(value: &impl Serialize) -> Result<u64, serde_json::Error> {
    struct HashWriter(u64);
    impl std::io::Write for HashWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            for &byte in bytes {
                self.0 ^= byte as u64;
                self.0 = self.0.wrapping_mul(0x100000001b3);
            }
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut writer = HashWriter(0xcbf29ce484222325);
    serde_json::to_writer(&mut writer, value)?;
    Ok(writer.0)
}
#[cfg(test)]
mod hash_tests {
    use super::*;
    #[test]
    fn streamed_json_hash_matches_original_bytes() {
        let message = Message::user_text("quotes \" / 羽毛\n");
        assert_eq!(
            json_hash(&message).unwrap(),
            fnv1a(&serde_json::to_vec(&message).unwrap())
        );
    }
}
pub fn json_to_pretty(v: &serde_json::Value) -> String {
    serde_json::to_string_pretty(v).unwrap_or_default()
}
