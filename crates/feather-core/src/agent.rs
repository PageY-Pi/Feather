//! The agent loop (pi 内核).
//!
//! One `run` = stream an assistant response → execute its tool calls →
//! append results → repeat until the response has no tool calls and no
//! queued follow-ups remain. Every completed turn commits a turn-granularity
//! snapshot to the session. The loop is provider-agnostic and drives both
//! the native GUI and headless/RPC mode.

use crate::compaction::{
    self, file_ops, shrink_kept_span, summary_message, CompactionSettings, SUMMARY_SYSTEM,
};
use crate::message::{Api, ContentBlock, Message, StopReason, ToolCall, Usage};
use crate::provider::{CacheHints, ChatRequest, StreamEvent, ThinkingLevel, ToolDef};
use crate::session::Session;
use crate::tokenizer;
use crate::tool::ToolRegistry;
use serde_json::Value;
use std::collections::VecDeque;
use std::sync::Arc;
/// Cache planning seam. feather ships exactly one implementation
/// (`feather-cache::CacheOptimizer`); it is compiled in and wired at
/// construction — there is no runtime registration.
/// One reading of the cache planner's running totals.
///
/// Kept as plain integers rather than a rate so the consumer decides what to show
/// and a division by zero cannot be silently baked in. `cache_read` counts prompt
/// tokens the provider served from its cache; `input` counts the ones it billed
/// fresh, which is exactly what the prefix strategy is trying to shrink.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CacheMetrics {
    pub requests: u64,
    pub input: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub output: u64,
    /// Length of the longest message prefix identical to the previous request.
    pub stable_prefix_len: u64,
}

impl CacheMetrics {
    /// Share of prompt tokens served from cache, in `0.0..=1.0`.
    ///
    /// `None` before any tokens have been billed: a request that has not happened
    /// yet has no hit rate, and reporting 0% would read as a cache miss.
    pub fn hit_rate(&self) -> Option<f64> {
        let served = self.input + self.cache_read;
        if served == 0 {
            None
        } else {
            Some(self.cache_read as f64 / served as f64)
        }
    }
}
pub trait CachePlanner: Send + Sync {
    /// Fill `cache` hints for the outgoing request and report history
    /// generation.
    fn plan(&self, req: &mut ChatRequest, history_hash: u64);
    /// Fast path for immutable Session context. Compatibility planners may
    /// ignore these scalar caches and use their original request-based method.
    fn plan_cached(
        &self,
        req: &mut ChatRequest,
        history_hash: u64,
        _message_hashes: &[u64],
        _prefix_tokens: &[u64],
    ) {
        self.plan(req, history_hash);
    }
    /// Record provider usage so hit-rate metrics accumulate.
    fn record(&self, usage: &Usage);
    /// The accumulated cache numbers, for the surface that shows them.
    ///
    /// `record` alone made the metrics write-only: `feather-cache` computed a hit
    /// rate and no caller ever read it, so whether the breakpoint placement
    /// actually helped was never observable. A planner that keeps no metrics
    /// returns `None` rather than zeroes, because "0% cached" and "not measured"
    /// are different claims and only one of them justifies a change.
    fn cache_metrics(&self) -> Option<CacheMetrics> {
        None
    }
    /// Mark the next `plan`/`plan_cached` call as a one-off (summarization) so
    /// it writes no cache entries. Callers must pair it with
    /// [`CachePlanner::end_one_off`]: the flag is per-request state, and a flag
    /// left set silently disables caching for the turn that follows.
    fn demote_one_off(&self);
    /// Clear a pending one-off demotion. Idempotent, and a no-op when the flag
    /// was never set, so it is safe on every exit path of a summarizing call.
    fn end_one_off(&self) {}
}

#[derive(Debug, Clone)]
pub struct AgentConfig {
    pub model: String,
    pub api: Api,
    pub system_prompt: String,
    pub max_tokens: u32,
    pub temperature: Option<f32>,
    pub context_window: u64,
    /// pi-style thinking level (None = off / model default).
    pub thinking_level: Option<ThinkingLevel>,
    pub compaction: CompactionSettings,
    /// model id for compaction summaries; falls back to `model` when unset.
    /// Only meaningful when the id is served by the same provider.
    pub compaction_model: Option<String>,
    /// Tool definitions (from the compiled-in registry).
    pub tools: Arc<Vec<ToolDef>>,
}

#[derive(Debug, Clone)]
pub enum AgentEvent {
    /// All audit rows up to this exclusive raw cursor are durable. Enables
    /// frontends to page/offload finalized rows during long tool-loop runs.
    AuditCommitted {
        raw_before: u64,
    },
    TurnStart(u64),
    /// Streaming deltas.
    ThinkingDelta(String),
    TextDelta(String),
    /// A streamed assistant attempt failed and a retry will restart the visible response.
    ResponseReset {
        attempt: usize,
        max_attempts: usize,
        delay_ms: u64,
        reason: String,
    },
    /// A finalized message. Assistant messages close the turn; user messages
    /// delivered from the steer/follow-up queues carry their image blocks so
    /// both the TUI and RPC echo attachments.
    AssistantMessage(Message),
    ToolStart {
        call: ToolCall,
    },
    ToolProgress {
        /// Shared, so a tool that streams hundreds of updates does not deep-clone
        /// its argument tree once per update.
        call: Arc<ToolCall>,
        text: String,
    },
    ToolEnd {
        call: ToolCall,
        text: String,
        is_error: bool,
        /// Structured side-channel from the tool (never sent to the model).
        /// The GUI reads `path` from it to label an edit card with the file it
        /// actually touched, which the anchor arguments alone never convey.
        details: Option<serde_json::Value>,
    },
    CompactionDone {
        tokens_before: u64,
        tokens_after: u64,
        chars_saved: usize,
    },
    TurnEnd(u64),
    RunEnd(RunResult),
    Notice(String),
    Error(String),
}

#[derive(Debug, Clone)]
pub struct RunResult {
    pub stop_reason: StopReason,
    pub turns: u64,
    pub usage: Usage,
    pub final_text: String,
}

#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error("provider: {0}")]
    Provider(#[from] crate::provider::ProviderError),
    #[error("session: {0}")]
    Session(#[from] crate::session::SessionError),
    #[error("aborted")]
    Aborted,
    #[error("no provider configured")]
    NoProvider,
}

#[derive(Clone)]
pub enum ProviderRef {
    Anthropic(Arc<crate::provider::anthropic::AnthropicProvider>),
    OpenAi(Arc<crate::provider::openai::OpenAiProvider>),
    Responses(Arc<crate::provider::responses::ResponsesProvider>),
    Pi(Arc<crate::provider::pi::PiProvider>),
    Faux(Arc<crate::provider::faux::FauxProvider>),
    #[cfg(test)]
    Scripted(Arc<test_support::ScriptedProvider>),
}

impl ProviderRef {
    async fn stream(
        &self,
        req: &ChatRequest,
    ) -> Result<crate::provider::ResponseStream, crate::provider::ProviderError> {
        match self {
            ProviderRef::Anthropic(p) => p.stream(req).await,
            ProviderRef::OpenAi(p) => p.stream(req).await,
            ProviderRef::Responses(p) => p.stream(req).await,
            ProviderRef::Pi(p) => p.stream(req).await,
            ProviderRef::Faux(p) => p.stream(req).await,
            #[cfg(test)]
            ProviderRef::Scripted(p) => p.stream(req).await,
        }
    }

    fn compaction_thinking(&self, active: Option<ThinkingLevel>) -> Option<ThinkingLevel> {
        match self {
            ProviderRef::OpenAi(p) => p.compaction_thinking(active),
            ProviderRef::Responses(p) => p.compaction_thinking(active),
            ProviderRef::Pi(p) => p.compaction_thinking(active),
            // Native Anthropic summaries retain their existing default/budget
            // behavior; the offline provider has no reasoning knobs.
            ProviderRef::Anthropic(_) | ProviderRef::Faux(_) => None,
            #[cfg(test)]
            ProviderRef::Scripted(_) => None,
        }
    }

    pub fn api(&self) -> Api {
        match self {
            ProviderRef::Anthropic(_) => Api::AnthropicMessages,
            ProviderRef::OpenAi(_) => Api::OpenaiCompletions,
            ProviderRef::Responses(p) => p.api(),
            ProviderRef::Pi(_) => Api::PiMessages,
            ProviderRef::Faux(_) => Api::Faux,
            #[cfg(test)]
            ProviderRef::Scripted(_) => Api::Faux,
        }
    }
}

/// Thread-safe steering/follow-up inbox. A supervisor can queue messages while
/// a provider future owns the mutable Agent; delivery happens at turn boundaries.
#[derive(Clone, Default)]
pub struct InputQueue(Arc<std::sync::Mutex<QueuedInput>>);
#[derive(Default)]
struct QueuedInput {
    steering: VecDeque<Message>,
    follow_up: VecDeque<Message>,
    steering_all: bool,
    follow_up_all: bool,
}
impl InputQueue {
    /// How many messages are still waiting to be delivered.
    ///
    /// Callers should read this instead of keeping their own counter: the queue
    /// is authoritative, and a mirror counter silently drifts whenever the
    /// agent is rebuilt or the queue is drained elsewhere.
    pub fn pending(&self) -> usize {
        let q = self.0.lock().unwrap_or_else(|e| e.into_inner());
        q.steering.len() + q.follow_up.len()
    }

    /// Move every pending message from `other` into `self`, preserving order and
    /// the steering/follow-up split.
    ///
    /// The inbox lives inside `Agent`, so rebuilding the agent (a model switch,
    /// for example) would otherwise discard input the user already submitted.
    /// After a failed run the queued prompts are deliberately left undelivered;
    /// losing them at that point looks like the app silently ate the message.
    pub fn adopt(&self, other: &InputQueue) {
        if std::ptr::eq(self, other) {
            return;
        }
        let (steering, follow_up, steering_all, follow_up_all) = {
            let mut o = other.0.lock().unwrap_or_else(|e| e.into_inner());
            (
                std::mem::take(&mut o.steering),
                std::mem::take(&mut o.follow_up),
                o.steering_all,
                o.follow_up_all,
            )
        };
        let mut q = self.0.lock().unwrap_or_else(|e| e.into_inner());
        q.steering.extend(steering);
        q.follow_up.extend(follow_up);
        if steering_all {
            q.steering_all = true;
        }
        if follow_up_all {
            q.follow_up_all = true;
        }
    }

    pub fn push(&self, message: Message, steering: bool) {
        let mut q = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if steering {
            q.steering.push_back(message);
        } else {
            q.follow_up.push_back(message);
        }
    }
    /// Recover a follow-up accepted just after a prompt task checked its inbox.
    pub fn pop_follow_up(&self) -> Option<Message> {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .follow_up
            .pop_front()
    }
    pub fn snapshot(&self) -> (Vec<String>, Vec<String>) {
        let q = self.0.lock().unwrap_or_else(|e| e.into_inner());
        (
            q.steering.iter().map(Message::text).collect(),
            q.follow_up.iter().map(Message::text).collect(),
        )
    }
    pub fn clear(&self) -> (Vec<String>, Vec<String>) {
        let mut q = self.0.lock().unwrap_or_else(|e| e.into_inner());
        (
            q.steering.drain(..).map(|m| m.text()).collect(),
            q.follow_up.drain(..).map(|m| m.text()).collect(),
        )
    }
    pub fn set_mode(&self, steering: bool, all: bool) {
        let mut q = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if steering {
            q.steering_all = all;
        } else {
            q.follow_up_all = all;
        }
    }
    pub fn modes(&self) -> (bool, bool) {
        let q = self.0.lock().unwrap_or_else(|e| e.into_inner());
        (q.steering_all, q.follow_up_all)
    }
    fn take(&self, steering: bool) -> Vec<Message> {
        let mut q = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let all = if steering {
            q.steering_all
        } else {
            q.follow_up_all
        };
        let queue = if steering {
            &mut q.steering
        } else {
            &mut q.follow_up
        };
        let count = if all { queue.len() } else { queue.len().min(1) };
        queue.drain(..count).collect()
    }
    fn prepend(&self, steering: bool, mut messages: Vec<Message>) {
        if messages.is_empty() {
            return;
        }
        let mut q = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let queue = if steering {
            &mut q.steering
        } else {
            &mut q.follow_up
        };
        while let Some(message) = messages.pop() {
            queue.push_front(message);
        }
    }
}

pub struct Agent {
    config: AgentConfig,
    session: Session,
    tools: Arc<ToolRegistry>,
    provider: ProviderRef,
    planner: Option<Arc<dyn CachePlanner>>,
    events: tokio::sync::broadcast::Sender<AgentEvent>,
    abort: tokio::sync::watch::Sender<bool>,
    abort_rx: tokio::sync::watch::Receiver<bool>,
    queue: InputQueue,
    /// System-prompt + tool-schema token cost for the current config. Fixed
    /// until `reconfigure`, so it is estimated once instead of every turn.
    static_request_tokens: u64,
    /// Learned correction between the token estimate and what providers bill.
    ///
    /// The estimator is a chars/4 heuristic, and measured against real sessions it
    /// is wrong in both directions: the first-turn ratio ranged from -20% to +29%
    /// over 24 sessions, with the median about -9%. A fixed safety margin cannot
    /// fix a two-sided error — it would push the overshoot cases further over the
    /// window while the undershoot cases keep their headroom. So the ratio is
    /// learned per session from the `Usage` every response reports, and kept in a
    /// narrow band so one pathological turn cannot swing the trigger.
    ///
    /// Only used where an error is dangerous: the compaction threshold, which must
    /// not let an over-window request through. Reporting and UI keep the raw
    /// estimate so a human sees the same number the heuristic produced.
    estimate_bias: EstimateBias,
}

/// The learned estimate/billed ratio, bounded and smoothed.
///
/// The bound is the important part. Unbounded feedback would let a session whose
/// provider bundles extra tokens (a large system prompt injected server-side, say)
/// ratchet the multiplier without limit, and the compaction threshold would drift
/// away from the model's real window. `MIN`/`MAX` keep the correction to the range
/// actually observed on real sessions, so it can only ever make a better guess
/// inside the regime the heuristic already lives in.
///
/// Smoothing is exponential because bills are noisy: one turn that happens to
/// carry a big tool result must not move the trigger. `ALPHA` weights a new
/// observation at 25%, so four turns dominate any single one.
#[derive(Debug, Clone, Copy)]
struct EstimateBias {
    /// Ratio of billed input to estimated input, smoothed. `1.0` = trust the estimate.
    ratio: f64,
    /// Observations folded in so far; the first one is taken whole.
    samples: u32,
}

impl EstimateBias {
    const MIN: f64 = 0.75;
    const MAX: f64 = 1.35;
    const ALPHA: f64 = 0.25;

    fn new() -> Self {
        Self {
            ratio: 1.0,
            samples: 0,
        }
    }

    /// Fold in one request's observed ratio.
    ///
    /// `estimated` is what the estimator said the request would cost; `billed` is
    /// what the provider reported. Both must be positive to say anything: a zero
    /// estimate means the counters were not yet populated, and a zero bill means
    /// the provider did not report usage.
    fn observe(&mut self, estimated: u64, billed: u64) {
        if estimated == 0 || billed == 0 {
            return;
        }
        let observed = (billed as f64 / estimated as f64).clamp(Self::MIN, Self::MAX);
        self.ratio = if self.samples == 0 {
            observed
        } else {
            self.ratio + (observed - self.ratio) * Self::ALPHA
        };
        self.ratio = self.ratio.clamp(Self::MIN, Self::MAX);
        self.samples = self.samples.saturating_add(1);
    }

    /// The estimate adjusted to what providers actually bill.
    ///
    /// Zero samples returns the estimate unchanged rather than pre-inflating it:
    /// guessing a margin before any observation is what a fixed constant would do,
    /// and the measurement showed that constant has no correct value.
    fn adjust(&self, estimate: u64) -> u64 {
        if self.samples == 0 {
            return estimate;
        }
        (estimate as f64 * self.ratio).round() as u64
    }
}

#[derive(Debug)]
struct AssistantStreamFailure {
    error: crate::provider::ProviderError,
    usage: Usage,
    published: bool,
}

#[cfg(test)]
mod test_support {
    use super::*;
    use bytes::Bytes;
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    pub enum ScriptItem {
        StartErr(crate::provider::ProviderError),
        Stream {
            events: Vec<StreamEvent>,
            terminal: Option<crate::provider::ProviderError>,
        },
    }

    pub struct ScriptedProvider {
        items: Mutex<VecDeque<ScriptItem>>,
        calls: AtomicUsize,
        pub(super) requests: Mutex<Vec<ChatRequest>>,
    }

    impl ScriptedProvider {
        pub fn new(items: Vec<ScriptItem>) -> Arc<Self> {
            Arc::new(Self {
                items: Mutex::new(items.into()),
                calls: AtomicUsize::new(0),
                requests: Mutex::new(Vec::new()),
            })
        }

        pub fn calls(&self) -> usize {
            self.calls.load(Ordering::Relaxed)
        }

        pub async fn stream(
            &self,
            req: &ChatRequest,
        ) -> Result<crate::provider::ResponseStream, crate::provider::ProviderError> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            self.requests.lock().unwrap().push(req.clone());
            let item = self
                .items
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .pop_front()
                .unwrap_or_else(|| ScriptItem::Stream {
                    events: vec![StreamEvent::TextDelta("exhausted".into())],
                    terminal: None,
                });
            match item {
                ScriptItem::StartErr(e) => Err(e),
                ScriptItem::Stream { events, terminal } => {
                    let mut chunks: Vec<Result<Bytes, crate::provider::ProviderError>> = events
                        .into_iter()
                        .map(|event| Ok(Bytes::from(format!("data: {}\n\n", encode(event)))))
                        .collect();
                    if let Some(e) = terminal {
                        chunks.push(Err(e));
                    }
                    Ok(crate::provider::ResponseStream::new(
                        Box::pin(ScriptedBytes {
                            chunks: chunks.into(),
                            pos: 0,
                        }),
                        Box::new(decode),
                    ))
                }
            }
        }
    }

    struct ScriptedBytes {
        chunks: VecDeque<Result<Bytes, crate::provider::ProviderError>>,
        pos: usize,
    }

    impl futures_util::Stream for ScriptedBytes {
        type Item = Result<Bytes, crate::provider::ProviderError>;

        fn poll_next(
            mut self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<Self::Item>> {
            let out = self.chunks.pop_front();
            self.pos += 1;
            std::task::Poll::Ready(out)
        }
    }

    fn encode(event: StreamEvent) -> String {
        match event {
            StreamEvent::TextDelta(t) => format!("text:{}", t.replace('\n', "\\n")),
            StreamEvent::ThinkingDelta(t) => format!("thinking:{}", t.replace('\n', "\\n")),
            StreamEvent::ToolCallStart { index, id, name } => {
                format!("toolstart:{}", serde_json::json!([index, id, name]))
            }
            StreamEvent::ToolCallDelta {
                index,
                json_fragment,
            } => format!("tooldelta:{}", serde_json::json!([index, json_fragment])),
            // The full usage, not just output: a scripted test that bills prompt
            // tokens has to be able to say so, and the prompt side is what the
            // estimate calibration and compaction threshold depend on. Without it a
            // test could not tell those features from dead code.
            StreamEvent::Usage(u) => format!(
                "usage:{}:{}:{}:{}",
                u.input, u.output, u.cache_read, u.cache_write
            ),
            StreamEvent::MessageEnd { stop_reason } => match stop_reason {
                StopReason::ToolUse => "end:tool".into(),
                StopReason::Length => "end:length".into(),
                StopReason::Aborted => "end:aborted".into(),
                StopReason::Error => "end:error".into(),
                StopReason::Stop => "end:stop".into(),
            },
            StreamEvent::FatalError(e) => format!("fatal:{e}"),
            _ => "end:stop".into(),
        }
    }

    fn decode(payload: &str, out: &mut Vec<StreamEvent>) {
        if let Some(t) = payload.strip_prefix("toolstart:") {
            let (index, id, name) = serde_json::from_str::<(usize, String, String)>(t).unwrap();
            out.push(StreamEvent::ToolCallStart { index, id, name });
            return;
        }
        if let Some(t) = payload.strip_prefix("tooldelta:") {
            let (index, json_fragment) = serde_json::from_str::<(usize, String)>(t).unwrap();
            out.push(StreamEvent::ToolCallDelta {
                index,
                json_fragment,
            });
            return;
        }
        if let Some(t) = payload.strip_prefix("text:") {
            out.push(StreamEvent::TextDelta(t.replace("\\n", "\n")));
        } else if let Some(t) = payload.strip_prefix("thinking:") {
            out.push(StreamEvent::ThinkingDelta(t.replace("\\n", "\n")));
        } else if let Some(n) = payload.strip_prefix("usage:") {
            // `input:output:cache_read:cache_write`, or a bare output count from the
            // older single-field form so existing scripts keep working.
            let parts: Vec<u64> = n.split(':').map(|p| p.parse().unwrap_or(0)).collect();
            out.push(StreamEvent::Usage(match parts.as_slice() {
                [output] => Usage {
                    output: *output,
                    ..Usage::default()
                },
                [input, output, cache_read, cache_write, ..] => Usage {
                    input: *input,
                    output: *output,
                    cache_read: *cache_read,
                    cache_write: *cache_write,
                },
                _ => Usage::default(),
            }));
        } else if let Some(reason) = payload.strip_prefix("end:") {
            let stop_reason = match reason {
                "tool" => StopReason::ToolUse,
                "length" => StopReason::Length,
                "aborted" => StopReason::Aborted,
                "error" => StopReason::Error,
                _ => StopReason::Stop,
            };
            out.push(StreamEvent::MessageEnd { stop_reason });
        } else if let Some(e) = payload.strip_prefix("fatal:") {
            out.push(StreamEvent::FatalError(e.to_string()));
        }
    }
}

impl Agent {
    pub fn new(
        config: AgentConfig,
        session: Session,
        tools: Arc<ToolRegistry>,
        provider: ProviderRef,
        planner: Option<Arc<dyn CachePlanner>>,
    ) -> Self {
        let static_request_tokens =
            tokenizer::estimate_request_static_tokens(&config.system_prompt, &config.tools);
        let (events, _) = tokio::sync::broadcast::channel(4096);
        let (abort, abort_rx) = tokio::sync::watch::channel(false);
        Self {
            config,
            session,
            tools,
            provider,
            planner,
            events,
            abort,
            abort_rx,
            queue: InputQueue::default(),
            static_request_tokens,
            estimate_bias: EstimateBias::new(),
        }
    }

    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<AgentEvent> {
        self.events.subscribe()
    }

    pub fn session(&self) -> &Session {
        &self.session
    }

    pub fn session_mut(&mut self) -> &mut Session {
        &mut self.session
    }

    pub fn config(&self) -> &AgentConfig {
        &self.config
    }

    /// The cache planner's running totals, if the planner keeps any.
    ///
    /// `None` when no planner is wired or the planner is not a measuring one;
    /// callers must treat that as "unknown", not "0%".
    pub fn cache_metrics(&self) -> Option<CacheMetrics> {
        self.planner.as_ref()?.cache_metrics()
    }

    pub fn abort(&self) {
        let _ = self.abort.send(true);
    }

    /// Clone of the abort signal sender — lets a supervisor (TUI thread,
    /// parent subagent) relay an abort into a run that holds `&mut self`.
    pub fn abort_sender(&self) -> tokio::sync::watch::Sender<bool> {
        self.abort.clone()
    }

    pub fn is_aborted(&self) -> bool {
        *self.abort_rx.borrow()
    }

    pub fn reset_abort(&mut self) {
        let _ = self.abort.send(false);
    }

    /// Queue a follow-up message to run after the current prompt completes.
    pub fn queue_follow_up(&mut self, text: impl Into<String>) {
        self.queue.push(Message::user_text(text.into()), false);
    }

    pub fn input_queue(&self) -> InputQueue {
        self.queue.clone()
    }
    pub fn set_auto_compaction(&mut self, enabled: bool) {
        self.config.compaction.enabled = enabled;
    }
    pub fn model_id(&self) -> String {
        self.config.model.clone()
    }

    /// pi's /think: change the thinking level at runtime.
    pub fn set_thinking_level(&mut self, level: Option<ThinkingLevel>) {
        self.config.thinking_level = level;
    }

    pub fn thinking_level(&self) -> Option<ThinkingLevel> {
        self.config.thinking_level
    }

    /// Replace runtime model/provider/tool config without changing session history or planner.
    pub fn reconfigure(
        &mut self,
        config: AgentConfig,
        provider: ProviderRef,
        tools: Arc<ToolRegistry>,
    ) {
        self.config = config;
        self.provider = provider;
        self.tools = tools;
        self.static_request_tokens = tokenizer::estimate_request_static_tokens(
            &self.config.system_prompt,
            &self.config.tools,
        );
        // A model switch is exactly when a broken pairing surfaces: the next
        // request goes to a provider that rejects a `tool` message with no
        // `tool_calls` parent, and the session looks permanently bricked. Repair
        // here so switching models can never carry the defect forward.
        self.session.repair_context("model switch");
    }

    /// pi's /fork: continue in a copy of the session (fresh id, same
    /// context); the original session stays untouched on disk.
    pub fn fork_session(&mut self) -> Result<String, crate::session::SessionError> {
        let s = self.session.fork()?;
        let id = s.id().to_string();
        self.session = s;
        Ok(id)
    }

    /// Prompt-side token estimate for the current context, assembled from the
    /// cached static part and the session's running totals. No message is
    /// visited, so this is safe to call before every request.
    fn request_input_tokens(&self) -> u64 {
        tokenizer::request_input_tokens_from_totals(
            self.config.api,
            self.static_request_tokens,
            (
                self.session.context_tokens(),
                self.session.context_tokens_without_thinking(),
            ),
        )
    }
    /// Build the next chat request from the linear history, applying the
    /// cache planner.
    pub fn build_request(&self) -> ChatRequest {
        let mut req = ChatRequest {
            system: self.config.system_prompt.clone(),
            messages: self.session.shared_messages(),
            tools: Arc::clone(&self.config.tools),
            model: self.config.model.clone(),
            max_tokens: self.config.max_tokens,
            temperature: self.config.temperature,
            cache: CacheHints::default(),
            thinking: self.config.thinking_level,
            session_id: self.session.id().to_string(),
        };
        if let Some(p) = &self.planner {
            let hash = self.session.history_hash();
            p.plan_cached(
                &mut req,
                hash,
                self.session.message_hashes(),
                self.session.prefix_tokens(),
            );
        }
        req
    }

    /// Submit a user prompt and run to completion (all turns).
    pub async fn prompt(&mut self, text: &str) -> Result<RunResult, AgentError> {
        self.prompt_message(Message::user_text(text.to_string()))
            .await
    }

    /// Prompt with pre-built content blocks — @mention attachments ride as
    /// native Text/Image blocks (pi's attachment pipeline).
    pub async fn prompt_message(&mut self, msg: Message) -> Result<RunResult, AgentError> {
        self.reset_abort();
        self.prompt_message_prepared(msg).await
    }

    /// Supervisor has reset abort before accepting the prompt. Do not reset
    /// again: an abort received between acceptance and first poll must survive.
    pub async fn prompt_message_prepared(&mut self, msg: Message) -> Result<RunResult, AgentError> {
        // Before the new prompt lands: the context must end on a complete
        // call/result pair. Repairing after this message would push a tool
        // result behind a user message, which providers reject in the opposite
        // direction. One cheap pass, and a no-op when the context is valid.
        self.session.repair_context("prompt");
        self.session.push(msg)?;
        let mut result = self.run_inner().await?;
        // follow-up queue: run each queued message as its own run
        while !self.is_aborted()
            && !matches!(result.stop_reason, StopReason::Aborted | StopReason::Error)
        {
            let mut next = self.queue.take(true);
            let steering = !next.is_empty();
            if next.is_empty() {
                next = self.queue.take(false);
            }
            if next.is_empty() {
                break;
            }
            for (index, message) in next.iter().enumerate() {
                if let Err(error) = self.session.push(message.clone()) {
                    // Only undelivered inputs go back to the inbox. Once a
                    // prompt is persisted, retrying it must not duplicate it.
                    self.queue.prepend(steering, next[index..].to_vec());
                    return Err(error.into());
                }
                let _ = self
                    .events
                    .send(AgentEvent::AssistantMessage(message.clone()));
            }
            let next_result = self.run_inner().await?;
            result.turns += next_result.turns;
            result.usage = crate::session::add_usage(result.usage, next_result.usage);
            result.stop_reason = next_result.stop_reason;
            result.final_text = next_result.final_text;
        }
        Ok(result)
    }

    /// Run one full agent run (possibly several turns).
    pub async fn run(&mut self) -> Result<RunResult, AgentError> {
        self.reset_abort();
        self.run_inner().await
    }

    async fn run_inner(&mut self) -> Result<RunResult, AgentError> {
        let mut total_usage = Usage::default();
        let mut final_text = String::new();
        let mut turns = 0u64;
        let stop;

        loop {
            if self.is_aborted() {
                stop = StopReason::Aborted;
                break;
            }
            // The provider limit applies to the complete request, not just the
            // cached message rows. Include system instructions and tool schemas,
            // and use only the configured compaction headroom before each normal request.
            // build_request so no later async work can make the prompt stale.
            // The threshold is the one place an estimate error is dangerous: too
            // low and an over-window request is sent and rejected. `adjust` folds in
            // what providers actually billed on earlier turns, which is the only
            // correction that holds across sessions — a fixed margin does not,
            // because the measured error runs in both directions.
            let request_tokens = self.estimate_bias.adjust(self.request_input_tokens());
            if compaction::should_compact_request(
                request_tokens,
                self.config.context_window,
                self.config.compaction.reserve_tokens,
            ) {
                let before_request_tokens = request_tokens;
                match self.compact("threshold").await {
                    Ok(saved) if saved > 0 => {
                        let after = self.request_input_tokens();
                        let _ = self.events.send(AgentEvent::CompactionDone {
                            tokens_before: before_request_tokens,
                            tokens_after: after,
                            chars_saved: saved,
                        });
                    }
                    Ok(_) => {
                        // A no-op compaction must not issue an over-limit request.
                        // Surface the condition rather than compacting repeatedly
                        // and sending the same invalid prompt three times.
                        let after = self.request_input_tokens();
                        if after > self.config.context_window {
                            return Err(AgentError::Provider(crate::provider::ProviderError::Stream(
                                format!(
                                    "context remains over model limit after compaction: {after} >= {}",
                                    self.config.context_window
                                ),
                            )));
                        }
                    }
                    Err(e) => {
                        // never kill the run for a failed compaction; surface
                        // it — but a deliberate abort is not a failure.
                        if !self.is_aborted() {
                            let _ = self
                                .events
                                .send(AgentEvent::Notice(format!("compaction failed: {e}")));
                        }
                    }
                }
            }

            if self.is_aborted() {
                stop = StopReason::Aborted;
                break;
            }
            turns += 1;
            let _ = self.events.send(AgentEvent::TurnStart(turns));

            // --- stream the assistant response ---
            let req = self.build_request();
            let msg = self.stream_assistant(&req, &mut total_usage, true).await?;
            // Release the immutable snapshot before Session::push so its
            // Arc<Vec<_>> remains unique: no copy-on-write of the full history.
            drop(req);

            let calls = msg.iter_tool_calls().cloned().collect::<Vec<_>>();
            let msg_stop = msg.stop_reason;
            final_text = msg.text();
            self.session.push(msg.clone())?;
            let _ = self.events.send(AgentEvent::AssistantMessage(msg));
            if calls.is_empty() || self.is_aborted() {
                // An abort DURING streaming leaves tool_use blocks without
                // results; committing them as-is makes the next request carry
                // an orphan tool_use (Anthropic 400s forever after).
                if !calls.is_empty() {
                    for call in &calls {
                        self.session.push(Message::tool_result(
                            call.id.clone(),
                            call.name.clone(),
                            "(aborted before execution)".to_string(),
                            true,
                        ))?;
                    }
                }
                self.commit_turn()?;
                let _ = self.events.send(AgentEvent::TurnEnd(turns));
                if !self.is_aborted() {
                    let steering = self.queue.take(true);
                    if !steering.is_empty() {
                        for message in steering {
                            let _ = self
                                .events
                                .send(AgentEvent::AssistantMessage(message.clone()));
                            self.session.push(message)?;
                        }
                        continue;
                    }
                }
                stop = if self.is_aborted() {
                    StopReason::Aborted
                } else {
                    // keep provider-reported Error/Length — collapsing to Stop
                    // made one-shot runs report provider failures as success
                    msg_stop.unwrap_or(StopReason::Stop)
                };
                break;
            }

            // --- execute tool calls in order, record results ---
            let mut index = 0;
            while index < calls.len() {
                if self.is_aborted() {
                    for call in &calls[index..] {
                        self.session.push(Message::tool_result(
                            call.id.clone(),
                            call.name.clone(),
                            "(aborted before execution)",
                            true,
                        ))?;
                    }
                    break;
                }
                if self
                    .tools
                    .get(&calls[index].name)
                    .is_some_and(|tool| tool.parallel_safe())
                {
                    let mut end = index + 1;
                    while end < calls.len()
                        && self
                            .tools
                            .get(&calls[end].name)
                            .is_some_and(|tool| tool.parallel_safe())
                    {
                        end += 1;
                    }
                    let mut jobs = tokio::task::JoinSet::new();
                    for (offset, call) in calls[index..end].iter().cloned().enumerate() {
                        let tools = self.tools.clone();
                        let abort = self.abort_rx.clone();
                        let events = self.events.clone();
                        jobs.spawn(async move {
                            (offset, execute_progress(tools, call, abort, events).await)
                        });
                    }
                    let mut outcomes = Vec::new();
                    while let Some(result) = jobs.join_next().await {
                        outcomes.push(result.map_err(|e| {
                            AgentError::Session(crate::session::SessionError::Io(
                                std::io::Error::other(e.to_string()),
                            ))
                        })?);
                    }
                    outcomes.sort_by_key(|(offset, _)| *offset);
                    for (offset, outcome) in outcomes {
                        let call = &calls[index + offset];
                        self.session.push(Message::tool_result_with_images(
                            call.id.clone(),
                            call.name.clone(),
                            outcome.text,
                            outcome.is_error,
                            outcome.images,
                        ))?;
                    }
                    index = end;
                } else {
                    let call = &calls[index];
                    let outcome = execute_progress(
                        self.tools.clone(),
                        call.clone(),
                        self.abort_rx.clone(),
                        self.events.clone(),
                    )
                    .await;
                    self.session.push(Message::tool_result_with_images(
                        call.id.clone(),
                        call.name.clone(),
                        outcome.text,
                        outcome.is_error,
                        outcome.images,
                    ))?;
                    index += 1;
                }
            }
            self.commit_turn()?;
            if !self.is_aborted() {
                for message in self.queue.take(true) {
                    let _ = self
                        .events
                        .send(AgentEvent::AssistantMessage(message.clone()));
                    self.session.push(message)?;
                }
            }
            let _ = self.events.send(AgentEvent::TurnEnd(turns));
            // tool results exist → another model request is required; loop
        }

        let result = RunResult {
            stop_reason: stop,
            turns,
            usage: total_usage,
            final_text,
        };
        let _ = self.events.send(AgentEvent::RunEnd(result.clone()));
        Ok(result)
    }

    /// Consume one provider response into a finalized assistant message.
    // Summary requests are maintenance: keep their deltas out of the live
    // response/event bus instead of relying on every frontend to filter them.
    async fn stream_assistant(
        &mut self,
        req: &ChatRequest,
        total_usage: &mut Usage,
        publish: bool,
    ) -> Result<Message, AgentError> {
        let mut abort = self.abort_rx.clone();
        let mut first_error_at: Option<tokio::time::Instant> = None;
        for failed_attempt in 0..crate::provider::retry::MAX_ATTEMPTS {
            match self.stream_assistant_once(req, publish).await {
                Ok((msg, usage)) => {
                    self.record_provider_usage(total_usage, usage, publish);
                    return Ok(msg);
                }
                Err(failure)
                    if matches!(failure.error, crate::provider::ProviderError::Aborted) =>
                {
                    self.record_provider_usage(total_usage, failure.usage, publish);
                    return Err(AgentError::Aborted);
                }
                Err(failure) => {
                    self.record_provider_usage(total_usage, failure.usage, publish);
                    let Some(plan) = crate::provider::retry::plan(&failure.error, failed_attempt)
                    else {
                        if publish {
                            let _ = self
                                .events
                                .send(AgentEvent::Error(failure.error.to_string()));
                        }
                        return Err(failure.error.into());
                    };
                    // Deadlines are anchored to the first error; subtract time
                    // already spent (including this failed request) so retries
                    // land at +5s/+10s/+30s/+60s rather than stacking sequential sleeps.

                    let first_error = *first_error_at.get_or_insert_with(tokio::time::Instant::now);
                    let delay = crate::provider::retry::delay_until(
                        &plan,
                        first_error,
                        tokio::time::Instant::now(),
                    );
                    if publish {
                        let _ = self.events.send(AgentEvent::ResponseReset {
                            attempt: plan.next_attempt,
                            max_attempts: crate::provider::retry::MAX_ATTEMPTS,
                            delay_ms: delay.as_millis() as u64,
                            reason: failure.error.to_string(),
                        });
                        if failure.published
                            && failed_attempt + 2 >= crate::provider::retry::MAX_ATTEMPTS
                        {
                            let _ = self
                                .events
                                .send(AgentEvent::Error(failure.error.to_string()));
                        }
                    }
                    // Failed usage has already been recorded; retry only the
                    // provider response. No assistant message has been persisted
                    // and no tool execution has started, so tool calls are not replayed.
                    crate::provider::retry::sleep_or_abort(delay, &mut abort).await?;
                    if failure.published {
                        // The ResponseReset event above is the frontend contract
                        // for discarding partial text/thinking from the failed attempt.
                    }
                }
            }
        }
        unreachable!("retry loop is bounded by provider::retry::MAX_ATTEMPTS")
    }

    async fn stream_assistant_once(
        &mut self,
        req: &ChatRequest,
        publish: bool,
    ) -> Result<(Message, Usage), AssistantStreamFailure> {
        let mut abort = self.abort_rx.clone();
        if *abort.borrow() {
            return Err(AssistantStreamFailure {
                error: crate::provider::ProviderError::Aborted,
                usage: Usage::default(),
                published: false,
            });
        }
        let request = self.provider.stream(req);
        let mut request = Box::pin(request);
        let mut stream = loop {
            tokio::select! {
                stream = &mut request => break stream.map_err(|error| AssistantStreamFailure { error, usage: Usage::default(), published: false })?,
                changed = abort.changed() => {
                    if changed.is_err() || *abort.borrow() {
                        return Err(AssistantStreamFailure { error: crate::provider::ProviderError::Aborted, usage: Usage::default(), published: false });
                    }
                }
            }
        };
        drop(request);
        let mut text = String::new();
        let mut thinking = String::new();
        let mut signature: Option<String> = None;
        let mut tools: Vec<(usize, Option<String>, String, String)> = Vec::new(); // (index, id, name, args)
        let mut usage = Usage::default();
        let mut stop_reason = StopReason::Stop;
        let mut aborted = false;
        let mut published = false;
        let mut saw_message_end = false;
        loop {
            if self.is_aborted() && !aborted {
                aborted = true;
                self.abort(); // stays set so tools observe it too
                drop(stream); // close the HTTP stream
                break;
            }
            let event = tokio::select! {
                event = stream.next() => event,
                changed = abort.changed() => {
                    if changed.is_err() || *abort.borrow() {
                        aborted = true;
                        self.abort();
                        break;
                    }
                    continue;
                }
            };
            match event {
                Some(Ok(StreamEvent::TextDelta(t))) => {
                    text.push_str(&t);
                    if publish {
                        published = true;
                        let _ = self.events.send(AgentEvent::TextDelta(t));
                    }
                }
                Some(Ok(StreamEvent::ThinkingDelta(t))) => {
                    thinking.push_str(&t);
                    if publish {
                        published = true;
                        let _ = self.events.send(AgentEvent::ThinkingDelta(t));
                    }
                }
                Some(Ok(StreamEvent::ThinkingSignatureDelta(s))) => {
                    // signature_delta may arrive in several chunks — append
                    signature.get_or_insert_with(String::new).push_str(&s);
                }
                Some(Ok(StreamEvent::ToolCallStart { index, id, name })) => {
                    // fill a placeholder slot (delta-before-start) instead of
                    // pushing a duplicate slot for the same index
                    match tools.iter_mut().find(|(i, ..)| *i == index) {
                        Some(slot) => {
                            if slot.1.is_none() {
                                slot.1 = Some(id);
                                slot.2 = name;
                            }
                        }
                        None => tools.push((index, Some(id), name, String::new())),
                    }
                }
                Some(Ok(StreamEvent::ToolCallDelta {
                    index,
                    json_fragment,
                })) => {
                    match tools.iter_mut().find(|(i, ..)| *i == index) {
                        Some(slot) => slot.3.push_str(&json_fragment),
                        // delta before Start (gateways that omit id): keep it
                        None => tools.push((index, None, String::new(), json_fragment)),
                    }
                }
                Some(Ok(StreamEvent::Usage(u))) => {
                    usage = crate::session::add_usage(usage, u);
                }
                Some(Ok(StreamEvent::MessageEnd { stop_reason: s })) => {
                    stop_reason = s;
                    saw_message_end = true;
                }
                Some(Ok(StreamEvent::FatalError(m))) => {
                    return Err(AssistantStreamFailure {
                        error: crate::provider::ProviderError::Stream(m),
                        usage,
                        published,
                    });
                }
                Some(Err(error)) => {
                    return Err(AssistantStreamFailure {
                        error,
                        usage,
                        published,
                    });
                }
                None => break,
            }
        }

        if !aborted && !saw_message_end {
            return Err(AssistantStreamFailure {
                error: crate::provider::ProviderError::Stream(
                    "response stream ended before message_end".into(),
                ),
                usage,
                published,
            });
        }
        let mut content: Vec<ContentBlock> = Vec::new();
        if !thinking.is_empty() {
            content.push(ContentBlock::Thinking {
                text: thinking,
                signature: signature.take(),
            });
        }
        if !text.is_empty() {
            content.push(ContentBlock::Text { text });
        }
        // tool calls in stream order
        tools.sort_by_key(|(i, ..)| *i);
        for (_, id, name, args) in tools {
            let parsed: Value = serde_json::from_str(args.trim()).unwrap_or_else(
                |e| serde_json::json!({ "_raw": args, "_parse_error": e.to_string() }),
            );
            content.push(ContentBlock::ToolCall(ToolCall {
                id: id.unwrap_or_else(|| crate::provider::gen_id("call_")),
                name,
                arguments: parsed,
            }));
        }
        if content.is_empty() && publish {
            content.push(ContentBlock::Text {
                text: if aborted {
                    "(aborted)".to_string()
                } else {
                    "(empty response)".to_string()
                },
            });
        }

        if aborted {
            stop_reason = StopReason::Aborted;
            // Final-stream errors are emitted by the retry supervisor.
        }

        Ok((
            Message::assistant(
                content,
                req.model.clone(),
                self.provider.api(),
                usage,
                stop_reason,
            ),
            usage,
        ))
    }

    /// Fold one request's estimate and bill into the learned correction.
    ///
    /// `billed` is `input + cache_read + cache_write`: every prompt token the
    /// provider accounted for, whether it charged full price or a cache rate. That
    /// is the quantity the context window limits, so it is the one the estimate
    /// has to track. `output` is excluded — it is not part of the prompt side and
    /// the threshold never counts it.
    fn observe_estimate(&mut self, billed: &Usage) {
        let prompt = billed.input + billed.cache_read + billed.cache_write;
        // The estimate of the request that produced this bill: the same function
        // the threshold uses, before any adjustment.
        let estimated = self.request_input_tokens();
        self.estimate_bias.observe(estimated, prompt);
    }

    /// `calibrates` says whether this request's bill may train `EstimateBias`.
    ///
    /// A summary request must not: it sends `SUMMARY_SYSTEM`, a single synthesized
    /// user message and *no tool schemas*, while `request_input_tokens` — the estimate
    /// it would be compared against — models the real system prompt plus 17 tool
    /// definitions. Feeding that pair to `observe` teaches the ratio a relationship
    /// that does not hold for any real turn, which is worse than one missing sample.
    fn record_provider_usage(&mut self, total_usage: &mut Usage, usage: Usage, calibrates: bool) {
        *total_usage = crate::session::add_usage(*total_usage, usage);
        self.session.add_usage(usage);
        // Only a request that billed prompt tokens says anything about the estimate.
        // A provider that reports zero (some gateways omit usage on errors) is ignored
        // by `observe` itself, so there is no second check here.
        if calibrates {
            self.observe_estimate(&usage);
        }
        if let Some(p) = &self.planner {
            p.record(&usage);
        }
    }

    fn commit_turn(&mut self) -> Result<u64, AgentError> {
        let turn = self.session.commit_turn()?;
        for diagnostic in self.session.take_runtime_diagnostics() {
            let _ = self.events.send(AgentEvent::Notice(diagnostic));
        }
        // Repairs are user-facing only: never fed back to the model.
        for note in self.session.take_notes() {
            let _ = self.events.send(AgentEvent::Notice(note));
        }
        if let Some(store) = self.session.transcript_store() {
            if let Ok(count) = store.raw_len() {
                let _ = self.events.send(AgentEvent::AuditCommitted {
                    raw_before: count as u64,
                });
            }
        }
        Ok(turn)
    }

    /// Compact the context: serialize (tool results shrunk), summarize, then
    /// rewrite the linear list to summary + kept span (tool results shrunk).
    /// Returns actual characters removed across summary + kept payloads; zero
    /// means no context replacement, not just "no tool results to shrink".
    pub async fn compact(&mut self, reason: &str) -> Result<usize, AgentError> {
        if self.is_aborted() {
            return Err(AgentError::Aborted);
        }
        let settings = self.config.compaction.clone();
        let before_request_tokens = self.request_input_tokens();
        let before_chars = compaction::context_characters(self.session.messages());
        let _before_tokens = self.session.context_tokens();
        // Tokens the request spends before the first message (system prompt and
        // tool schemas), and the room messages may occupy before the trigger
        // fires again. Cut and safety-net shrink both use this, so a rewrite
        // never lands exactly on the trigger and re-fires on the next turn.
        let static_request_tokens = self.static_request_tokens;
        // Budget the rewrite must hit. `context_window - reserve` alone is the
        // trigger *line*: a rewrite that lands on it is over it again as soon as
        // one tool result arrives, so every later turn compacts.
        let trigger = self
            .config
            .context_window
            .saturating_sub(settings.reserve_tokens);
        let room = compaction::compaction_room(trigger, static_request_tokens, &settings);
        let cut = self.compaction_cut(&settings, room);
        if cut == 0 {
            // No user boundary to summarize over (a single-span session, e.g.
            // one long tool-loop turn or a giant paste). cut == 0 alone does
            // not mean the context is over the window — a healthy short
            // session also has cut 0 when everything fits in the recent
            // budget — so only fall back to in-place elision when the request
            // truly exceeds the window. Otherwise this would degrade to
            // mangling history on every turn. The durable raw audit is never
            // touched by elision.
            if before_request_tokens <= self.config.context_window {
                return Ok(0);
            }
            let budget = room;
            let mut shrunk = self.session.messages().to_vec();
            compaction::shrink_messages_to_tokens(&mut shrunk, budget);
            let saved = before_chars.saturating_sub(compaction::context_characters(&shrunk));
            if saved == 0 {
                return Ok(0); // nothing removable at all; caller reports over-limit
            }
            self.replace_messages(shrunk);
            return Ok(saved);
        }
        let context = self.session.shared_messages();
        let mut kept = compaction::without_thinking(&context[cut..]);
        let to_summarize = &context[..cut];

        // The one-off demotion is applied at the request itself (below), not
        // here: this function has several early returns between this point and
        // the summary request, and a flag set here would leak onto the next real
        // turn if any of them fired.
        let serialized = compaction::serialize_messages(
            to_summarize.iter().filter(|m| {
                !(m.role == crate::message::Role::User
                    && m.text_cow().starts_with("## Conversation summary"))
            }),
            settings.summary_tool_result_budget,
        );
        let previous = to_summarize
            .iter()
            .rev()
            .find(|m| {
                m.role == crate::message::Role::User
                    && m.text_cow().starts_with("## Conversation summary")
            })
            .map(|m| m.text_cow());
        let mut prompt = String::from("Summarize this coding-agent conversation.\n");
        if let Some(prev) = previous {
            prompt.push_str("\nPrevious summary (iterate on it):\n");
            prompt.push_str(&prev);
            prompt.push('\n');
        }
        if reason != "manual" {
            prompt.push_str("\nReason: automatic (context budget).\n");
        }
        prompt.push_str("\n<conversation>\n");
        prompt.push_str(&serialized);
        prompt.push_str("\n</conversation>");

        let sum_req = ChatRequest {
            system: SUMMARY_SYSTEM.to_string(),
            messages: vec![Message::user_text(prompt)].into(),
            tools: Vec::new().into(),
            model: self
                .config
                .compaction_model
                .clone()
                .unwrap_or_else(|| self.config.model.clone()),
            max_tokens: settings.reserve_tokens.min(8192) as u32,
            temperature: None,
            cache: CacheHints::default(),
            thinking: self
                .provider
                .compaction_thinking(self.config.thinking_level),
            session_id: self.session.id().to_string(),
        };
        let mut _sink = Usage::default();
        // The summary request is a one-off: it must not write cache entries, and
        // it must not disturb the prefix tracking the next real turn depends on.
        // The flag is per-request state, so it is cleared as soon as the request
        // returns — on the failure paths too, which is why the result is bound
        // before `?` rather than propagated directly.
        let attempt = {
            if let Some(p) = &self.planner {
                p.demote_one_off();
            }
            let result = self.stream_assistant(&sum_req, &mut _sink, false).await;
            if let Some(p) = &self.planner {
                p.end_one_off();
            }
            result
        };
        let summary_msg = attempt?;
        // A cancelled summarization must not rewrite the context. `stream_assistant`
        // already recorded the usage on the session, so no extra add here.
        if self.is_aborted() || summary_msg.stop_reason == Some(StopReason::Aborted) {
            return Err(AgentError::Aborted);
        }
        let summary = summary_msg.text();
        if summary.trim().is_empty()
            || summary_msg.stop_reason == Some(StopReason::Error)
            || summary_msg.iter_tool_calls().next().is_some()
        {
            return Err(crate::provider::ProviderError::Stream(
                "compaction produced no usable summary; context left unchanged".into(),
            )
            .into());
        }

        // rewrite the linear context: summary + shrunk kept span
        let (reads, writes) = file_ops(to_summarize);
        // What this summary replaces, so the fold stays traceable after the
        // original messages leave the live context.
        let folded = compaction::FoldedRange {
            messages: to_summarize.len(),
            from_ms: to_summarize.first().map(|m| m.timestamp).unwrap_or(0),
            to_ms: to_summarize.last().map(|m| m.timestamp).unwrap_or(0),
        };
        drop(context); // release old immutable history before installing new context
        shrink_kept_span(&mut kept, &settings);
        let mut new_msgs = vec![summary_message(&summary, &reads, &writes, folded)];

        new_msgs.extend(kept);
        // Safety net only: the cut above already targets the keep budget, so
        // this must stay a no-op in the normal case.
        compaction::shrink_messages_to_tokens(&mut new_msgs, room);
        let after_chars = compaction::context_characters(&new_msgs);
        // `new_msgs` is a candidate list that is not installed yet, so the
        // session totals do not describe it: this one estimate has to scan.
        let after_request_tokens = tokenizer::estimate_request_input_tokens(
            self.config.api,
            &self.config.system_prompt,
            &new_msgs,
            &self.config.tools,
        );
        let saved = before_chars.saturating_sub(after_chars);
        if saved == 0 || after_request_tokens >= before_request_tokens {
            return Ok(0); // oversized/ineffective summary: preserve original context
        }
        // replace the whole linear list; raw transcript keeps originals
        self.replace_messages(new_msgs);
        Ok(saved)
    }

    /// Cut point for compaction, in messages.
    ///
    /// Turn-granular cutting protects the newest completed answer's whole user
    /// span, which is what keeps a rewritten session readable. One prompt that
    /// runs a long tool loop can make that single span most of the context
    /// though: protecting it then leaves no working room, compaction saves
    /// almost nothing, and the next turn compacts again. Protect the span only
    /// while it still leaves a keep-budget of room; otherwise cut inside the
    /// oversized turn at boundaries that keep call/result pairs intact.
    fn compaction_cut(&self, settings: &CompactionSettings, room: u64) -> usize {
        let messages = self.session.messages();
        // Two candidate cuts. Display-first keeps the newest answer's whole user
        // span readable; the budget cut keeps only a bounded span and summarizes
        // the oversized work behind it. Display-first is affordable only when the
        // span it protects fits the room available before the trigger.
        let protected =
            compaction::plan_cut_without_thinking(messages, settings.keep_recent_tokens);
        let budget_cut = compaction::plan_cut_budget(messages, settings.keep_recent_tokens);
        // Mid-turn the "next user message" that display-first snaps to is the
        // prompt appended at the tail, so its kept span can collapse to one row.
        // That passes any size check while summarizing the very work the caller
        // is watching, so treat "nothing meaningful kept" as unaffordable.
        let protects_work = messages[protected..]
            .iter()
            .any(|m| m.role != crate::message::Role::User || m.text_cow().len() > 200);
        // Measure what the *installed* context would cost: `compact` shrinks old
        // tool results in the kept span, so charging the raw size would reject a
        // rewrite that is affordable in practice.
        let mut provisional = messages[protected..].to_vec();
        compaction::shrink_kept_span(&mut provisional, settings);
        let protected_kept: u64 = provisional
            .iter()
            .map(tokenizer::estimate_message_tokens_without_thinking)
            .sum();
        if protected > 0 && protects_work && protected_kept <= room {
            return protected;
        }
        budget_cut
    }

    fn replace_messages(&mut self, msgs: Vec<Message>) {
        self.session.set_messages_for_compaction(msgs);
        // commit a snapshot so rewind/compaction interplay stays consistent
        if let Err(e) = self.commit_turn() {
            // a failed snapshot means crash-resume lands BEFORE compaction —
            // never swallow that silently
            let _ = self.events.send(AgentEvent::Notice(format!(
                "snapshot commit after compaction failed: {e}"
            )));
        }
    }
}

async fn execute_progress(
    tools: Arc<ToolRegistry>,
    call: ToolCall,
    abort: tokio::sync::watch::Receiver<bool>,
    events: tokio::sync::broadcast::Sender<AgentEvent>,
) -> crate::tool::ToolOutcome {
    let Some(tool) = tools.get(&call.name) else {
        return crate::tool::ToolOutcome::err(tools.unknown_tool_error(&call.name));
    };
    let _ = events.send(AgentEvent::ToolStart { call: call.clone() });
    // A streaming tool can emit hundreds of updates, and every `ToolProgress`
    // used to deep-clone the whole `ToolCall` — including a `serde_json::Value`
    // argument tree that readers never look at. Sharing one `Arc` makes each
    // update a refcount bump instead.
    let shared: Arc<ToolCall> = Arc::new(call.clone());
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let fut = tool.execute_with_progress(call.arguments.clone(), abort, tx);
    tokio::pin!(fut);
    let outcome = loop {
        tokio::select! {
            result = &mut fut => break result,
            Some(text) = rx.recv() => {
                let _ = events.send(AgentEvent::ToolProgress {
                    call: Arc::clone(&shared),
                    text,
                });
            }
        }
    };
    while let Ok(text) = rx.try_recv() {
        let _ = events.send(AgentEvent::ToolProgress {
            call: Arc::clone(&shared),
            text,
        });
    }
    let _ = events.send(AgentEvent::ToolEnd {
        call,
        text: outcome.text.clone(),
        is_error: outcome.is_error,
        details: outcome.details.clone(),
    });
    outcome
}

#[cfg(test)]
mod retry_tests {
    #[test]
    fn request_build_shares_large_history_and_tool_definitions() {
        let mut agent = Agent::new(
            config(),
            Session::memory(std::path::Path::new("fixture"), "request-share"),
            Arc::new(ToolRegistry::new()),
            ProviderRef::Scripted(test_support::ScriptedProvider::new(Vec::new())),
            None,
        );
        agent
            .session_mut()
            .push(Message::user_text("large payload".repeat(50_000)))
            .unwrap();
        let request = agent.build_request();
        let second = agent.build_request();
        assert!(std::ptr::eq(
            request.messages.as_slice().as_ptr(),
            agent.session().messages().as_ptr()
        ));
        assert!(Arc::ptr_eq(&request.messages, &second.messages));
        assert!(Arc::ptr_eq(&request.tools, &second.tools));
    }

    /// A summary request must not train the estimate correction.
    ///
    /// Its system prompt, message list and (empty) tool set are all different from the
    /// request `request_input_tokens` models, so the ratio between the two is
    /// meaningless — and because a summary is billed in full while the estimate covers
    /// the *real* prompt plus every tool schema, it lands well off 1.0. Letting it
    /// through would bias the next turn's compaction threshold on a request that will
    /// never be sent again.
    #[test]
    fn a_summary_request_does_not_calibrate_the_estimate() {
        let mut agent = Agent::new(
            config(),
            Session::memory(std::path::Path::new("fixture"), "summary-bias"),
            Arc::new(ToolRegistry::new()),
            ProviderRef::Scripted(test_support::ScriptedProvider::new(Vec::new())),
            None,
        );
        let raw = agent.request_input_tokens();
        let mut total = Usage::default();
        // What a summary bills: its own tiny prompt, nothing like `raw`.
        let summary_bill = Usage {
            input: 40_000,
            output: 100,
            ..Usage::default()
        };

        agent.record_provider_usage(&mut total, summary_bill, false);
        assert_eq!(
            agent.estimate_bias.samples, 0,
            "a summary request must not count as a calibration sample"
        );
        assert_eq!(
            agent.estimate_bias.adjust(raw),
            raw,
            "and must not move the multiplier"
        );
        // Its cost still lands in the session totals: not calibrating is not the same
        // as not accounting.
        assert_eq!(total.input, 40_000);

        // A real turn does calibrate, so the gate is not simply "never learn".
        agent.record_provider_usage(&mut total, summary_bill, true);
        assert_eq!(agent.estimate_bias.samples, 1);
        assert!(
            agent.estimate_bias.ratio > 1.0,
            "40 000 billed against a small estimate must raise the ratio, got {}",
            agent.estimate_bias.ratio
        );
    }

    /// The learned correction has to change the compaction decision, or it is dead
    /// code that only looks like a safeguard.
    ///
    /// The provider here bills far more than the estimator predicts — the real
    /// pattern for a session whose system prompt carries a large project context the
    /// estimator under-counts. After one billed turn, the same context must be
    /// judged closer to its real size.
    #[tokio::test(start_paused = true)]
    async fn a_billed_turn_moves_the_compaction_threshold() {
        let root = std::env::temp_dir().join(crate::provider::gen_id("bias-agent"));
        std::fs::create_dir_all(&root).unwrap();
        // Every estimate is scaled by the same factor to make the comparison exact:
        // the assertion is on the *estimate*, not on whether compaction fired at a
        // particular context window (which would depend on the prompt's size).
        let billed = 40_000u64;
        let provider = test_support::ScriptedProvider::new(vec![
            test_support::ScriptItem::Stream {
                events: vec![
                    StreamEvent::Usage(Usage {
                        input: billed,
                        output: 1,
                        ..Usage::default()
                    }),
                    StreamEvent::TextDelta("done".into()),
                    StreamEvent::MessageEnd {
                        stop_reason: StopReason::Stop,
                    },
                ],
                terminal: None,
            },
            test_support::ScriptItem::Stream {
                events: vec![
                    StreamEvent::Usage(Usage {
                        input: billed,
                        output: 1,
                        ..Usage::default()
                    }),
                    StreamEvent::TextDelta("again".into()),
                    StreamEvent::MessageEnd {
                        stop_reason: StopReason::Stop,
                    },
                ],
                terminal: None,
            },
        ]);
        let mut agent = Agent::new(
            config(),
            Session::memory(&root, "bias"),
            Arc::new(ToolRegistry::new()),
            ProviderRef::Scripted(provider.clone()),
            None,
        );
        agent
            .session_mut()
            .push(Message::user_text("x".repeat(40_000)))
            .unwrap();
        let raw = agent.request_input_tokens();
        assert_eq!(
            agent.estimate_bias.adjust(raw),
            raw,
            "before any bill there is nothing learned to apply"
        );

        agent
            .prompt_message(Message::user_text("go".to_string()))
            .await
            .unwrap();

        let adjusted = agent.estimate_bias.adjust(raw);
        assert!(
            adjusted > raw,
            "a provider billing {billed} against a {raw} estimate must raise the \
             correction; got {adjusted} (raw {raw}, ratio {:.3})",
            agent.estimate_bias.ratio
        );
        // And it must not over-shoot: the bound is what keeps the threshold near the
        // model's real window instead of chasing a single observation.
        assert!(
            adjusted <= (raw as f64 * 1.35).round() as u64,
            "the correction must stay inside the measured band, got {adjusted}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn illegal_tool_error_is_visible_in_next_request_and_run_continues() {
        struct Blocked;
        impl crate::tool::AgentTool for Blocked {
            fn name(&self) -> &'static str {
                "blocked"
            }
            fn description(&self) -> &'static str {
                "Synthetic blocked command"
            }
            fn parameters(&self) -> serde_json::Value {
                serde_json::json!({"type":"object"})
            }
            fn execute<'a>(
                &'a self,
                _: serde_json::Value,
                _: tokio::sync::watch::Receiver<bool>,
            ) -> crate::tool::BoxFut<'a> {
                Box::pin(async {
                    crate::tool::ToolOutcome::err("[ILLEGAL_COMMAND] command blocked before execution; choose an allowed alternative")
                })
            }
        }
        let root = std::env::temp_dir().join(crate::provider::gen_id("illegal-agent"));
        std::fs::create_dir_all(&root).unwrap();
        let provider = test_support::ScriptedProvider::new(vec![
            test_support::ScriptItem::Stream {
                events: vec![
                    StreamEvent::ToolCallStart {
                        index: 0,
                        id: "blocked-1".into(),
                        name: "blocked".into(),
                    },
                    StreamEvent::ToolCallDelta {
                        index: 0,
                        json_fragment: "{}".into(),
                    },
                    StreamEvent::MessageEnd {
                        stop_reason: StopReason::ToolUse,
                    },
                ],
                terminal: None,
            },
            test_support::ScriptItem::Stream {
                events: vec![
                    StreamEvent::TextDelta("corrected safely".into()),
                    StreamEvent::MessageEnd {
                        stop_reason: StopReason::Stop,
                    },
                ],
                terminal: None,
            },
        ]);
        let mut tools = ToolRegistry::new();
        tools.add(Arc::new(Blocked));
        let mut cfg = config();
        cfg.tools = tools.defs().into();
        let mut agent = Agent::new(
            cfg,
            Session::memory(&root, "illegal"),
            Arc::new(tools),
            ProviderRef::Scripted(provider.clone()),
            None,
        );
        let result = agent.prompt("try operation").await.unwrap();
        assert_eq!(result.final_text, "corrected safely");
        assert_eq!(provider.calls(), 2);
        let requests = provider.requests.lock().unwrap();
        assert!(requests[1]
            .messages
            .iter()
            .any(|m| m.role == crate::message::Role::ToolResult
                && m.is_error
                && m.text().contains("ILLEGAL_COMMAND")));
        drop(requests);
        std::fs::remove_dir_all(root).unwrap();
    }
    use super::*;
    use crate::compaction::CompactionSettings;
    use crate::message::Api;
    use crate::provider::ProviderError;
    use crate::tool::ToolRegistry;

    fn config() -> AgentConfig {
        AgentConfig {
            model: "scripted".into(),
            api: Api::Faux,
            system_prompt: "test".into(),
            max_tokens: 128,
            temperature: None,
            context_window: 200_000,
            thinking_level: None,
            compaction: CompactionSettings::default(),
            compaction_model: None,
            tools: Vec::new().into(),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn retries_three_times_resets_partial_stream_and_counts_failed_usage() {
        let root = std::env::temp_dir().join(crate::provider::gen_id("retry-agent"));
        std::fs::create_dir_all(&root).unwrap();
        let provider = test_support::ScriptedProvider::new(vec![
            test_support::ScriptItem::Stream {
                events: vec![
                    StreamEvent::Usage(Usage {
                        output: 2,
                        ..Usage::default()
                    }),
                    StreamEvent::TextDelta("partial".into()),
                ],
                terminal: Some(ProviderError::Stream("transient one".into())),
            },
            test_support::ScriptItem::StartErr(ProviderError::Http("transient two".into())),
            test_support::ScriptItem::Stream {
                events: vec![StreamEvent::Usage(Usage {
                    output: 4,
                    ..Usage::default()
                })],
                terminal: Some(ProviderError::Stream("transient three".into())),
            },
            test_support::ScriptItem::Stream {
                events: vec![
                    StreamEvent::Usage(Usage {
                        output: 3,
                        ..Usage::default()
                    }),
                    StreamEvent::TextDelta("ok".into()),
                    StreamEvent::MessageEnd {
                        stop_reason: StopReason::Stop,
                    },
                ],
                terminal: None,
            },
        ]);
        let mut agent = Agent::new(
            config(),
            Session::memory(&root, "retry-test"),
            Arc::new(ToolRegistry::new()),
            ProviderRef::Scripted(provider.clone()),
            None,
        );
        let mut events = agent.subscribe();

        let result = agent.prompt("hi").await.unwrap();
        assert_eq!(result.final_text, "ok");
        assert_eq!(provider.calls(), 4);
        assert_eq!(result.usage.output, 9);
        assert_eq!(agent.session().messages().len(), 2);
        assert_eq!(agent.session().messages()[1].text(), "ok");

        let mut resets = 0;
        let mut saw_partial = false;
        let mut saw_ok = false;
        while let Ok(event) = events.try_recv() {
            match event {
                AgentEvent::TextDelta(t) if t == "partial" => saw_partial = true,
                AgentEvent::TextDelta(t) if t == "ok" => saw_ok = true,
                AgentEvent::ResponseReset { delay_ms, .. } => {
                    resets += 1;
                    // Deadlines are anchored to the first error: +5s, +10s, +30s and +60s.
                    // i.e. remaining waits are measured from that first error.
                    assert!(delay_ms <= 60_000);
                }
                _ => {}
            }
        }
        assert!(saw_partial);
        assert!(saw_ok);
        assert_eq!(resets, 3);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test(start_paused = true)]
    async fn retries_fatal_error_event_and_premature_eof() {
        let root = std::env::temp_dir().join(crate::provider::gen_id("retry-fatal-eof"));
        std::fs::create_dir_all(&root).unwrap();
        let provider = test_support::ScriptedProvider::new(vec![
            test_support::ScriptItem::Stream {
                events: vec![
                    StreamEvent::TextDelta("fatal partial".into()),
                    StreamEvent::FatalError("fatal midstream".into()),
                ],
                terminal: None,
            },
            test_support::ScriptItem::Stream {
                events: vec![StreamEvent::TextDelta("eof partial".into())],
                terminal: None,
            },
            test_support::ScriptItem::Stream {
                events: vec![
                    StreamEvent::TextDelta("ok".into()),
                    StreamEvent::MessageEnd {
                        stop_reason: StopReason::Stop,
                    },
                ],
                terminal: None,
            },
        ]);
        let mut agent = Agent::new(
            config(),
            Session::memory(&root, "retry-fatal-eof"),
            Arc::new(ToolRegistry::new()),
            ProviderRef::Scripted(provider.clone()),
            None,
        );
        let mut events = agent.subscribe();

        let result = agent.prompt("hi").await.unwrap();
        assert_eq!(result.final_text, "ok");
        assert_eq!(provider.calls(), 3);
        assert_eq!(agent.session().messages().len(), 2);

        let mut resets = 0;
        while let Ok(event) = events.try_recv() {
            if let AgentEvent::ResponseReset { .. } = event {
                resets += 1;
            }
        }
        assert_eq!(resets, 2);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test(start_paused = true)]
    async fn final_published_failure_emits_error_but_does_not_persist_partial() {
        let root = std::env::temp_dir().join(crate::provider::gen_id("retry-final-fail"));
        std::fs::create_dir_all(&root).unwrap();
        let provider = test_support::ScriptedProvider::new(vec![
            test_support::ScriptItem::StartErr(ProviderError::Http("transient one".into())),
            test_support::ScriptItem::StartErr(ProviderError::Http("transient two".into())),
            test_support::ScriptItem::StartErr(ProviderError::Http("transient three".into())),
            test_support::ScriptItem::Stream {
                events: vec![StreamEvent::TextDelta("do not persist".into())],
                terminal: Some(ProviderError::Stream("final fail".into())),
            },
        ]);
        let mut agent = Agent::new(
            config(),
            Session::memory(&root, "retry-final-fail"),
            Arc::new(ToolRegistry::new()),
            ProviderRef::Scripted(provider.clone()),
            None,
        );
        let mut events = agent.subscribe();

        let err = agent.prompt("hi").await.unwrap_err();
        assert!(matches!(
            err,
            AgentError::Provider(ProviderError::Stream(_))
        ));
        assert_eq!(provider.calls(), 5);
        assert_eq!(agent.session().messages().len(), 1);
        assert_eq!(agent.session().messages()[0].text(), "hi");

        let mut saw_partial = false;
        let mut saw_error = false;
        while let Ok(event) = events.try_recv() {
            match event {
                AgentEvent::TextDelta(t) if t == "do not persist" => saw_partial = true,
                AgentEvent::Error(e) if e.contains("final fail") => saw_error = true,
                AgentEvent::AssistantMessage(m) if m.text().contains("do not persist") => {
                    panic!("partial failure was finalized into context: {m:?}");
                }
                // Four retry deadlines mean five total provider calls.
                _ => {}
            }
        }
        assert!(saw_partial);
        assert!(saw_error);
        let _ = std::fs::remove_dir_all(root);
    }
    #[tokio::test(start_paused = true)]
    async fn quota_exhaustion_is_not_retried() {
        let root = std::env::temp_dir().join(crate::provider::gen_id("retry-quota"));
        std::fs::create_dir_all(&root).unwrap();
        let provider =
            test_support::ScriptedProvider::new(vec![test_support::ScriptItem::StartErr(
                ProviderError::Api {
                    status: 429,
                    body: "insufficient_quota".into(),
                },
            )]);
        let mut agent = Agent::new(
            config(),
            Session::memory(&root, "quota-test"),
            Arc::new(ToolRegistry::new()),
            ProviderRef::Scripted(provider.clone()),
            None,
        );
        let err = agent.prompt("hi").await.unwrap_err();
        assert!(matches!(
            err,
            AgentError::Provider(ProviderError::Api { .. })
        ));
        assert_eq!(provider.calls(), 1);
        let _ = std::fs::remove_dir_all(root);
    }
}

/// The learned correction has to be conservative in exactly the direction that
/// matters: it may raise the estimate (compacting early) but must never let an
/// over-window request through on the strength of one noisy turn.
#[cfg(test)]
mod estimate_bias_tests {
    use super::EstimateBias;

    #[test]
    fn an_unobserved_bias_leaves_the_estimate_alone() {
        // Pre-inflating before any measurement is the fixed-margin mistake: the
        // measurement showed the error runs in both directions, so there is no
        // constant that is right. Until a bill arrives, trust the estimate.
        let bias = EstimateBias::new();
        assert_eq!(bias.adjust(1000), 1000);
    }

    #[test]
    fn the_first_observation_is_taken_whole() {
        // A session whose first bill is 20% over the estimate should be corrected
        // immediately, not after a long warm-up: the very next request could be the
        // one that crosses the window.
        let mut bias = EstimateBias::new();
        bias.observe(1000, 1200);
        assert_eq!(bias.adjust(1000), 1200);
    }

    #[test]
    fn a_single_outlier_cannot_swing_the_correction() {
        // Smoothing: one turn carrying a huge tool result must not move the
        // trigger by the full amount. After a 1.0 baseline, a 2.0 observation
        // lands at 1.25 rather than 2.0.
        let mut bias = EstimateBias::new();
        bias.observe(1000, 1000);
        bias.observe(1000, 2000);
        let adjusted = bias.adjust(1000);
        assert!(
            adjusted > 1000 && adjusted < 1400,
            "one outlier must not move the estimate to the outlier: got {adjusted}"
        );
    }

    #[test]
    fn the_correction_stays_inside_the_measured_band() {
        // Unbounded feedback would let a provider that bundles server-side tokens
        // ratchet the multiplier without limit, drifting the compaction threshold
        // away from the model's real window.
        let mut bias = EstimateBias::new();
        for _ in 0..100 {
            bias.observe(1000, 100_000);
        }
        assert!(
            bias.adjust(1000) <= 1350,
            "the correction must stay bounded, got {}",
            bias.adjust(1000)
        );
        let mut low = EstimateBias::new();
        for _ in 0..100 {
            low.observe(1000, 1);
        }
        assert!(
            low.adjust(1000) >= 750,
            "an under-billing provider must not collapse the estimate, got {}",
            low.adjust(1000)
        );
    }

    #[test]
    fn zero_usage_teaches_nothing() {
        // Some gateways omit usage on error responses. Folding a zero in would read
        // as "this request cost nothing" and drag the correction down.
        let mut bias = EstimateBias::new();
        bias.observe(1000, 0);
        assert_eq!(bias.adjust(1000), 1000);
        bias.observe(0, 1000);
        assert_eq!(bias.adjust(1000), 1000);
    }

    #[test]
    fn the_correction_converges_toward_a_steady_undercount() {
        // The real-world case: the estimator is consistently ~15% low because the
        // provider tokenizer splits differently. After a few turns the correction
        // must be close, which is the whole point of learning it per session.
        let mut bias = EstimateBias::new();
        for _ in 0..12 {
            bias.observe(10_000, 11_500);
        }
        let adjusted = bias.adjust(10_000);
        assert!(
            (11_000..=11_600).contains(&adjusted),
            "expected to converge near the billed 11 500, got {adjusted}"
        );
    }
}
