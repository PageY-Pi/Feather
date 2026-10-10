//! Provider layer: enum-dispatched chat providers with streaming.
//!
//! Providers are compiled in: Anthropic Messages, OpenAI-compatible chat
//! completions, OpenAI/Codex Responses, and the offline deterministic `Faux`
//! provider used by tests and demos.

pub mod anthropic;
pub mod compat;
pub mod credentials;
pub mod faux;
pub mod openai;
pub mod responses;
pub mod pi;
pub mod retry;
pub mod transport;

use crate::message::{StopReason, Usage};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ProviderError {
    #[error("authentication error: {0}")]
    Auth(String),
    #[error("http error: {0}")]
    Http(String),
    #[error("provider returned error {status}: {body}")]
    Api { status: u16, body: String },
    #[error("stream decode error: {0}")]
    Decode(String),
    #[error("provider stream error: {0}")]
    Stream(String),
    #[error("aborted")]
    Aborted,
}

/// A tool definition sent to the model (JSON-schema parameters).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

/// Where the cache-optimizer wants cache breakpoints placed (Anthropic
/// `cache_control`; OpenAI-compat providers ignore it — prefix caching is
/// automatic there, which is why message order stability matters everywhere).
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
pub struct CacheHints {
    /// Mark the system prompt block for cache write.
    pub system: bool,
    /// Mark the last tool definition for cache write.
    pub tools: bool,
    /// Index (into `messages`) of the last message of the stable prefix;
    /// `None` means no message-level breakpoint.
    pub stable_message_index: Option<usize>,
    /// Provider-neutral sticky routing/retention preference, mirrored from the
    /// installed pi-cache-optimizer concepts. Providers that do not understand
    /// it ignore it.
    pub sticky_ttl_seconds: Option<u64>,
    /// Minimum new suffix budget before writing a fresh message breakpoint.
    pub min_breakpoint_tokens: u64,
}

impl Default for CacheHints {
    fn default() -> Self {
        Self {
            system: false,
            tools: false,
            stable_message_index: None,
            sticky_ttl_seconds: None,
            min_breakpoint_tokens: 1024,
        }
    }
}

/// pi's thinking levels (`THINKING_LEVELS = ["off","minimal","low","medium",
/// "high","xhigh","max"]`). `None` on a request means "model default / off".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThinkingLevel {
    Off,
    Minimal,
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

impl ThinkingLevel {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "off" => Self::Off,
            "minimal" => Self::Minimal,
            "low" => Self::Low,
            "medium" | "med" => Self::Medium,
            "high" => Self::High,
            "xhigh" => Self::Xhigh,
            "max" => Self::Max,
            _ => return None,
        })
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Minimal => "minimal",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Xhigh => "xhigh",
            Self::Max => "max",
        }
    }

    /// Anthropic budget tokens per level (pi's anthropic-messages provider:
    /// `budget_tokens`, clamped to `max_tokens - 1024`; minimal stays at the
    /// provider fallback 1024).
    pub fn budget_tokens(&self) -> u32 {
        match self {
            Self::Off => 0,
            Self::Minimal => 1024,
            Self::Low => 2048,
            Self::Medium => 8192,
            // pi-ai simple-options clamps xhigh/max to high for budget APIs.
            Self::High | Self::Xhigh | Self::Max => 16384,
        }
    }
}

/// A fully assembled chat request.
#[derive(Debug, Clone)]
pub struct ChatRequest {
    pub system: String,
    pub messages: std::sync::Arc<Vec<crate::message::Message>>,
    pub tools: std::sync::Arc<Vec<ToolDef>>,
    pub model: String,
    pub max_tokens: u32,
    pub temperature: Option<f32>,
    pub cache: CacheHints,
    /// Thinking level (pi semantics: None = model default / off).
    pub thinking: Option<ThinkingLevel>,
    /// Session id for provider-side cache routing (pi-cache-optimizer
    /// alignment: one-way hash → `prompt_cache_key` on OpenAI-compatible
    /// payloads; empty = omit).
    pub session_id: String,
}

/// Incremental events of one streamed assistant response.
/// Incremental events of one streamed assistant response.
#[derive(Debug, Clone)]
pub enum StreamEvent {
    TextDelta(String),
    ThinkingDelta(String),
    /// Signature of the assistant thinking block (Anthropic `signature_delta`).
    /// Must be echoed back with the thinking text on follow-up turns.
    ThinkingSignatureDelta(String),
    ToolCallStart {
        index: usize,
        id: String,
        name: String,
    },
    ToolCallDelta {
        index: usize,
        json_fragment: String,
    },
    /// Usage increment for this response so far (input/cache arrive in
    /// `message_start`, output in `message_delta`); accumulate, don't replace.
    Usage(Usage),
    /// Terminal event of a response.
    MessageEnd {
        stop_reason: StopReason,
    },
    /// The provider reported a hard error mid-stream. The response pump
    /// converts this into `Err(ProviderError::Stream)` so callers see a
    /// failure instead of a truncated-but-"successful" response.
    FatalError(String),
}

/// Route-aware transport helpers. Provider structs keep their historical
/// client field, but streaming requests fetch the current route at send time.
pub(crate) use transport::{http_post_sse, shared_client};

/// A provider response stream.
impl ResponseStream {
    pub(crate) fn new(inner: BoxStream, handler: EventHandler) -> Self {
        Self {
            inner,
            handler_events: Vec::new(),
            parser: SseParser::new(),
            handler,
            pending_err: None,
        }
    }

    /// Run the provider payload handler, then lift any FatalError out of the
    /// queue: earlier events stay deliverable, the error surfaces as Err
    /// once they drain.
    fn run_handler(&mut self, payload: &str) {
        if self.pending_err.is_some() { return; }
        (self.handler)(payload, &mut self.handler_events);
        for event in self.handler_events.drain(..) {
            match event {
                StreamEvent::FatalError(message) => {
                    self.pending_err = Some(ProviderError::Stream(message));
                    break; // retain only the events before the first fatal error
                }
                event => self.parser.queue.push_back(event),
            }
        }
    }

    pub async fn next(&mut self) -> Option<Result<StreamEvent, ProviderError>> {
        loop {
            // drain buffered events first
            if !self.parser.queue.is_empty() {
                return Some(Ok(self.parser.queue.pop_front().expect("queue is nonempty")));
            }
            if let Some(err) = self.pending_err.take() {
                return Some(Err(err));
            }
            let chunk = match crate::poll_stream_next(&mut self.inner).await {
                Some(Ok(b)) => b,
                Some(Err(e)) => return Some(Err(e)),
                None => {
                    // byte stream closed: flush any pending SSE payload
                    for payload in self.parser.finish() {
                        self.run_handler(&payload);
                    }
                    if self.parser.queue.is_empty() {
                        if let Some(err) = self.pending_err.take() {
                            return Some(Err(err));
                        }
                        return None;
                    }
                    continue;
                }
            };
            for payload in self.parser.feed(&chunk) {
                self.run_handler(&payload);
            }
        }
    }
}
/// Provider-specific SSE payload handler.
pub type EventHandler = Box<dyn FnMut(&str, &mut Vec<StreamEvent>) + Send>;

/// A provider response stream: boxed byte stream + SSE parser + a
/// provider-specific payload handler.
pub struct ResponseStream {
    handler_events: Vec<StreamEvent>,
    pub(crate) inner: BoxStream,
    pub(crate) parser: SseParser,
    pub(crate) handler: EventHandler,
    /// deferred terminal error: surfaced as Err after earlier events drain
    pub(crate) pending_err: Option<ProviderError>,
}

type BoxStream =
    std::pin::Pin<Box<dyn futures_util::Stream<Item = Result<bytes::Bytes, ProviderError>> + Send>>;

/// Minimal incremental SSE parser: splits on `\n`, collects `data:` lines,
/// yields complete payloads on blank-line boundaries. Handles `\r\n` and
/// multi-line data (joined with `\n`) like the spec says.
pub struct SseParser {
    buf: Vec<u8>,
    scanned: usize,
    data: String,
    queue: std::collections::VecDeque<StreamEvent>,
}

impl Default for SseParser {
    fn default() -> Self {
        Self::new()
    }
}

impl SseParser {
    pub fn new() -> Self {
        Self {
            buf: Vec::new(),
            scanned: 0,
            data: String::new(),
            queue: std::collections::VecDeque::new(),
        }
    }

    /// Feed bytes, return complete `data` payloads.
    pub fn feed(&mut self, chunk: &[u8]) -> Vec<String> {
        self.buf.extend_from_slice(chunk);
        let mut out = Vec::new();
        let mut start = 0;
        let mut search = self.scanned;
        while let Some(relative_end) = self.buf[search..].iter().position(|&b| b == b'\n') {
            let end = search + relative_end;
            // UTF-8 is decoded only after the complete line arrived. Borrow the
            // common valid-UTF-8 case, and compact the byte buffer once/feed.
            let line = String::from_utf8_lossy(&self.buf[start..end]);
            let line = line.trim_end_matches('\r');
            if line.is_empty() {
                if !self.data.is_empty() {
                    out.push(std::mem::take(&mut self.data));
                }
            } else if let Some(rest) = line.strip_prefix("data:") {
                let rest = rest.strip_prefix(' ').unwrap_or(rest);
                if !self.data.is_empty() { self.data.push('\n'); }
                self.data.push_str(rest);
            }
            start = end + 1;
            search = start;
        }
        if start > 0 { self.buf.drain(..start); }
        self.scanned = self.buf.len(); // don't rescan an unfinished long line
        out
    }

    /// Call after the byte stream ends: process any pending line, then flush.
    pub fn finish(&mut self) -> Vec<String> {
        self.scanned = 0;
        if !self.buf.is_empty() {
            let line_bytes: Vec<u8> = std::mem::take(&mut self.buf);
            let line = String::from_utf8_lossy(&line_bytes);
            let line = line.trim_end_matches('\r');
            if let Some(rest) = line.strip_prefix("data:") {
                let rest = rest.strip_prefix(' ').unwrap_or(rest);
                if self.data.is_empty() {
                    self.data.push_str(rest);
                } else {
                    self.data.push('\n');
                    self.data.push_str(rest);
                }
            }
        }
        if self.data.is_empty() {
            Vec::new()
        } else {
            vec![std::mem::take(&mut self.data)]
        }
    }
}

/// Generate a unique-enough tool-call id without external crates.
pub fn gen_id(prefix: &str) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let c = COUNTER.fetch_add(1, Ordering::Relaxed);
    let t = crate::message::now_millis();
    format!("{prefix}{:012x}{:04x}", t, c & 0xffff)
}


#[cfg(test)]
mod tests {
    #[test]
    fn fragmented_utf8_crlf_and_many_lines_preserve_payload_order() {
        let text="data: 羽毛\r\ndata: next\r\n\r\n";let bytes=text.as_bytes();
        for cut in 0..=bytes.len() {
            let mut parser=SseParser::new();let mut out=parser.feed(&bytes[..cut]);out.extend(parser.feed(&bytes[cut..]));out.extend(parser.finish());
            assert_eq!(out,vec!["羽毛\nnext"]);
        }
        let mut parser=SseParser::new();let chunk=(0..10_000).map(|n|format!("data: {n}\n\n")).collect::<String>();
        let out=parser.feed(chunk.as_bytes());assert_eq!(out.len(),10_000);
        for (n,payload) in out.iter().enumerate(){assert_eq!(payload,&n.to_string());}
        assert!(parser.finish().is_empty());
        let mut parser=SseParser::new();let long=format!("data: {}","x".repeat(65536));
        for chunk in long.as_bytes().chunks(7){assert!(parser.feed(chunk).is_empty());}
        assert_eq!(parser.feed(b"\n\n"),vec!["x".repeat(65536)]);
    }
    #[tokio::test]
    async fn event_queue_is_fifo_and_fatal_ends_after_prior_events() {
        let input=futures_util::stream::iter(vec![Ok(bytes::Bytes::from_static(b"data: first\n\ndata: fatal\n\ndata: ignored\n\n"))]);
        let handler:EventHandler=Box::new(|payload,out|{
            if payload=="fatal" {out.push(StreamEvent::FatalError("synthetic".into()));out.push(StreamEvent::TextDelta("also ignored".into()));}
            else {out.push(StreamEvent::TextDelta(payload.into()));}
        });
        let mut stream=ResponseStream::new(Box::pin(input),handler);
        assert!(matches!(stream.next().await,Some(Ok(StreamEvent::TextDelta(ref text))) if text=="first"));
        assert!(matches!(stream.next().await,Some(Err(ProviderError::Stream(ref text))) if text=="synthetic"));
        assert!(stream.next().await.is_none());
    }
    use super::*;

    #[test]
    fn sse_parses_multiline_and_crlf() {
        let mut p = SseParser::new();
        let mut out = p.feed(b"data: {\"a\":1}\r\n\r\ndata: par");
        assert_eq!(out, vec!["{\"a\":1}"]);
        // payload split across chunks; the line completes at the newline and
        // the event at the blank line (SSE continuation semantics)
        out = p.feed(b"tial\n\ndata: second\n\n");
        assert_eq!(out, vec!["partial", "second"]);
        out = p.feed(b"data: end");
        assert!(out.is_empty());
        out = p.finish();
        assert_eq!(out, vec!["end"]);
    }
}
