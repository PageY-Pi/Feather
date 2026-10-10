//! Deterministic offline provider.
//!
//! A `FauxProvider` replays a script of turns — text and/or tool calls —
//! with no network and no API key. The same bytes come out on every run,
//! which is what makes the kernel testable offline (borrowed from
//! bigfish1913/pi-rust's faux-provider philosophy: first-class, not a mock
//! bolted on).

use super::{ChatRequest, EventHandler, ProviderError, ResponseStream};
use crate::message::{StopReason, Usage};

/// One scripted assistant turn.
#[derive(Debug, Clone)]
pub enum FauxTurn {
    Text(String),
    /// (tool name, arguments JSON)
    ToolCall(String, serde_json::Value),
    /// text plus trailing tool call in one response
    TextThen {
        text: String,
        tool: String,
        arguments: serde_json::Value,
    },
}

#[derive(Debug, Clone, Default)]
pub struct FauxScript {
    pub turns: Vec<FauxTurn>,
}

impl FauxScript {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_text(mut self, text: impl Into<String>) -> Self {
        self.turns.push(FauxTurn::Text(text.into()));
        self
    }

    pub fn with_tool(mut self, name: impl Into<String>, arguments: serde_json::Value) -> Self {
        self.turns.push(FauxTurn::ToolCall(name.into(), arguments));
        self
    }

    pub fn with_text_then(
        mut self,
        text: impl Into<String>,
        tool: impl Into<String>,
        arguments: serde_json::Value,
    ) -> Self {
        self.turns.push(FauxTurn::TextThen {
            text: text.into(),
            tool: tool.into(),
            arguments,
        });
        self
    }
}

pub struct FauxProvider {
    script: std::sync::Mutex<FauxScript>,
    cursor: std::sync::atomic::AtomicUsize,
}

impl FauxProvider {
    pub fn new(script: FauxScript) -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self {
            script: std::sync::Mutex::new(script),
            cursor: std::sync::atomic::AtomicUsize::new(0),
        })
    }

    fn next_turn(&self) -> Option<FauxTurn> {
        let c = self
            .cursor
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.script
            .lock()
            .unwrap_or_else(|e| e.into_inner()) // poison-tolerant
            .turns
            .get(c)
            .cloned()
    }
}

/// A fully in-memory "HTTP" response so the SSE machinery is exercised
/// end-to-end offline.
pub struct FauxByteStream {
    chunks: Vec<bytes::Bytes>,
    pos: usize,
}

impl futures_util::Stream for FauxByteStream {
    type Item = Result<bytes::Bytes, ProviderError>;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        if self.pos < self.chunks.len() {
            let c = self.chunks[self.pos].clone();
            self.pos += 1;
            std::task::Poll::Ready(Some(Ok(c)))
        } else {
            std::task::Poll::Ready(None)
        }
    }
}

pub(crate) fn faux_stream(
    script_turn: FauxTurn,
    _model: &str,
) -> Result<ResponseStream, ProviderError> {
    let mut sse = String::new();
    let mut usage = Usage::default();
    let mut stop = StopReason::Stop;

    let mut push = |e: &str| {
        sse.push_str("data: ");
        sse.push_str(e);
        sse.push_str("\n\n");
    };

    let tool_index = 0usize;
    match script_turn {
        FauxTurn::Text(t) => {
            let toks = crate::tokenizer::estimate_tokens(&t).max(1);
            usage.output = toks;
            push(&format!(
                r#"{{"type":"content_block_delta","index":0,"delta":{{"type":"text_delta","text":{}}}}}"#,
                serde_json::to_string(&t).unwrap_or_default()
            ));
        }
        FauxTurn::ToolCall(name, args) => {
            let id = super::gen_id("call_");
            usage.output = 16;
            stop = StopReason::ToolUse;
            push(&format!(
                r#"{{"type":"content_block_start","index":{tool_index},"content_block":{{"type":"tool_use","id":"{id}","name":"{name}"}}}}"#
            ));
            push(&format!(
                r#"{{"type":"content_block_delta","index":{tool_index},"delta":{{"type":"input_json_delta","partial_json":{}}}}}"#,
                serde_json::to_string(&args.to_string()).unwrap_or_default()
            ));
        }
        FauxTurn::TextThen {
            text,
            tool,
            arguments,
        } => {
            let id = super::gen_id("call_");
            usage.output = crate::tokenizer::estimate_tokens(&text) + 16;
            stop = StopReason::ToolUse;
            push(&format!(
                r#"{{"type":"content_block_delta","index":0,"delta":{{"type":"text_delta","text":{}}}}}"#,
                serde_json::to_string(&text).unwrap_or_default()
            ));
            push(&format!(
                r#"{{"type":"content_block_start","index":1,"content_block":{{"type":"tool_use","id":"{id}","name":"{tool}"}}}}"#
            ));
            push(&format!(
                r#"{{"type":"content_block_delta","index":1,"delta":{{"type":"input_json_delta","partial_json":{}}}}}"#,
                serde_json::to_string(&arguments.to_string()).unwrap_or_default()
            ));
        }
    }
    push(&format!(
        r#"{{"type":"message_delta","delta":{{"stop_reason":"{}"}},"usage":{{"input_tokens":{},"output_tokens":{},"cache_read_input_tokens":0,"cache_creation_input_tokens":0}}}}"#,
        match stop {
            StopReason::ToolUse => "tool_use",
            _ => "end_turn",
        },
        usage.input,
        usage.output
    ));
    sse.push_str("data: [DONE]\n\n");

    let chunks: Vec<bytes::Bytes> = sse
        .as_bytes()
        .chunks(64)
        .map(bytes::Bytes::copy_from_slice)
        .collect();
    let handler: EventHandler = Box::new(super::anthropic::anthropic_event);
    Ok(ResponseStream::new(
        Box::pin(FauxByteStream { chunks, pos: 0 }),
        handler,
    ))
}

impl FauxProvider {
    /// Stream the next scripted turn as a response stream.
    pub async fn stream(&self, req: &ChatRequest) -> Result<ResponseStream, ProviderError> {
        let turn = self
            .next_turn()
            .unwrap_or(FauxTurn::Text("(faux script exhausted)".to_string()));
        faux_stream(turn, &req.model)
    }
}
