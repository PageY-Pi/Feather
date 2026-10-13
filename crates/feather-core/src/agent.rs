//! The agent loop (pi 内核).
//!
//! One `run` = stream an assistant response → execute its tool calls →
//! append results → repeat until the response has no tool calls and no
//! queued follow-ups remain. Every completed turn commits a turn-granularity
//! snapshot to the session. The loop is provider-agnostic and drives both
//! the native GUI and headless/RPC mode.

use crate::compaction::{
    self, file_ops, should_compact, shrink_kept_span, summary_message, CompactionSettings,
    SUMMARY_SYSTEM,
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
    /// One-off requests (summaries) must not write cache entries.
    fn demote_one_off(&self);
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
        call: ToolCall,
        text: String,
    },
    ToolEnd {
        call: ToolCall,
        text: String,
        is_error: bool,
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
            StreamEvent::Usage(u) => format!("usage:{}", u.output),
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
            out.push(StreamEvent::Usage(Usage {
                output: n.parse().unwrap_or(0),
                ..Usage::default()
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
    }

    /// pi's /fork: continue in a copy of the session (fresh id, same
    /// context); the original session stays untouched on disk.
    pub fn fork_session(&mut self) -> Result<String, crate::session::SessionError> {
        let s = self.session.fork()?;
        let id = s.id().to_string();
        self.session = s;
        Ok(id)
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
            // --- compaction check before the next model request ---
            let ctx_tokens = self.session.context_tokens();
            if should_compact(
                ctx_tokens,
                self.config.context_window,
                &self.config.compaction,
            ) {
                match self.compact("threshold").await {
                    Ok(saved) if saved > 0 => {
                        let after = self.session.context_tokens();
                        let _ = self.events.send(AgentEvent::CompactionDone {
                            tokens_before: ctx_tokens,
                            tokens_after: after,
                            chars_saved: saved,
                        });
                    }
                    Ok(_) => {} // no-op: do not announce a context rewrite
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
                        self.session.push(Message::tool_result(
                            call.id.clone(),
                            call.name.clone(),
                            outcome.text,
                            outcome.is_error,
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
                    self.session.push(Message::tool_result(
                        call.id.clone(),
                        call.name.clone(),
                        outcome.text,
                        outcome.is_error,
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
                    self.record_provider_usage(total_usage, usage);
                    return Ok(msg);
                }
                Err(failure)
                    if matches!(failure.error, crate::provider::ProviderError::Aborted) =>
                {
                    self.record_provider_usage(total_usage, failure.usage);
                    return Err(AgentError::Aborted);
                }
                Err(failure) => {
                    self.record_provider_usage(total_usage, failure.usage);
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

    fn record_provider_usage(&mut self, total_usage: &mut Usage, usage: Usage) {
        *total_usage = crate::session::add_usage(*total_usage, usage);
        self.session.add_usage(usage);
        if let Some(p) = &self.planner {
            p.record(&usage);
        }
    }

    fn commit_turn(&mut self) -> Result<u64, AgentError> {
        let turn = self.session.commit_turn()?;
        for diagnostic in self.session.take_runtime_diagnostics() {
            let _ = self.events.send(AgentEvent::Notice(diagnostic));
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
        let before_chars = compaction::context_characters(self.session.messages());
        let before_tokens = self.session.context_tokens();
        let cut = compaction::plan_cut_without_thinking(
            self.session.messages(),
            settings.keep_recent_tokens,
        );
        if cut == 0 {
            return Ok(0); // nothing to summarize
        }
        let context = self.session.shared_messages();
        let mut kept = compaction::without_thinking(&context[cut..]);
        let to_summarize = &context[..cut];

        // one-off request: cache writes disabled
        if let Some(p) = &self.planner {
            p.demote_one_off();
        }
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
        let summary_msg = self.stream_assistant(&sum_req, &mut _sink, false).await?;
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
        drop(context); // release old immutable history before installing new context
        shrink_kept_span(&mut kept, &settings);
        let mut new_msgs = vec![summary_message(&summary, &reads, &writes)];
        new_msgs.extend(kept);
        let after_chars = compaction::context_characters(&new_msgs);
        let after_tokens = tokenizer::estimate_context_tokens(&new_msgs);
        let saved = before_chars.saturating_sub(after_chars);
        if saved == 0 || after_tokens >= before_tokens {
            return Ok(0); // oversized/ineffective summary: preserve original context
        }
        // replace the whole linear list; raw transcript keeps originals
        self.replace_messages(new_msgs);
        Ok(saved)
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
        return crate::tool::ToolOutcome::err(format!("unknown tool: {}", call.name));
    };
    let _ = events.send(AgentEvent::ToolStart { call: call.clone() });
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let fut = tool.execute_with_progress(call.arguments.clone(), abort, tx);
    tokio::pin!(fut);
    let outcome = loop {
        tokio::select! {
            result = &mut fut => break result,
            Some(text) = rx.recv() => { let _ = events.send(AgentEvent::ToolProgress { call: call.clone(), text }); }
        }
    };
    while let Ok(text) = rx.try_recv() {
        let _ = events.send(AgentEvent::ToolProgress {
            call: call.clone(),
            text,
        });
    }
    let _ = events.send(AgentEvent::ToolEnd {
        call,
        text: outcome.text.clone(),
        is_error: outcome.is_error,
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
