//! feather-core — the pi kernel in Rust.
//!
//! Library-first: the agent loop, linear session with turn-granularity
//! snapshots, streaming providers, compaction and the static tool registry
//! all live here with no I/O assumptions beyond async — the same code drives
//! the TUI binary, one-shot mode and tests.

pub mod agent;
pub mod compaction;
pub mod database;
pub mod file_snapshots;
pub mod gitckpt;
pub mod message;
pub mod process;
pub mod provider;
pub mod records;
pub mod session;
pub mod session_catalog;
pub mod session_store;
pub mod snapshot;
pub mod substore;
pub mod tokenizer;
pub mod tool;
pub mod workspace_path;

pub use agent::{Agent, AgentConfig, AgentEvent, RunResult};
pub use message::{ContentBlock, Message, Role, StopReason, ToolCall, Usage};
pub use provider::{
    anthropic::AnthropicProvider, faux::FauxProvider, openai::OpenAiProvider, CacheHints,
    ChatRequest, ProviderError, ResponseStream, StreamEvent, ToolDef,
};
pub use session::Session;
pub use session_store::{SessionMeta, SessionStore};
pub use snapshot::SnapshotStore;
pub use tool::{tool_display_label, AgentTool, ToolOutcome, ToolRegistry};

/// Poll a boxed byte stream once (shim to keep futures-util out of our
/// public API).
pub(crate) async fn poll_stream_next(
    s: &mut std::pin::Pin<
        Box<
            dyn futures_util::Stream<Item = Result<bytes::Bytes, crate::provider::ProviderError>>
                + Send,
        >,
    >,
) -> Option<Result<bytes::Bytes, crate::provider::ProviderError>> {
    std::future::poll_fn(|cx| s.as_mut().poll_next(cx)).await
}
