//! feather-cache — the cache-optimizer.
//!
//! Prompt caches bill by *prefix stability*, so the optimizer's job is to
//! make every request byte-identical to the previous one except for the
//! suffix that actually changed, and to tell the provider where the stable
//! prefix ends:
//!
//! 1. **Deterministic ordering** — tool defs arrive from `ToolRegistry`
//!    already sorted; the system prompt is fixed; the session is linear and
//!    append-only, so history never rewrites under the optimizer.
//! 2. **Stable-prefix tracking** — per-message content hashes let the
//!    optimizer find the longest message prefix identical to the previous
//!    request. The prefix end gets the message-level cache breakpoint.
//! 3. **Breakpoint placement** — Anthropic gets `cache_control` markers on
//!    (a) the system prompt, (b) the last tool definition, and (c) the last
//!    stable-prefix message. The retained TTL and the minimum growth before a
//!    fresh message breakpoint are `CacheSettings` (mirroring
//!    pi-cache-optimizer's conservative defaults). OpenAI-compatible gateways
//!    prefix-cache implicitly; stability does the work, so no markers are
//!    needed there.
//! 4. **One-off demotion** — compaction summaries must not pollute the
//!    cache: `demote_one_off()` clears breakpoints for the next request.
//! 5. **Metrics** — provider usage is accumulated into rolling cache
//!    hit-rate / write numbers surfaced by `/cost` and the status bar.
//!
//! # pi-cache-optimizer parity boundary
//!
//! The pure, host-independent half of the installed `pi-cache-optimizer`
//! plugin is ported in [`plugin`] (retention gate, Anthropic cache-control
//! TTL ordering/repair, OpenAI `prompt_cache_key` payload helpers, session
//! key derivation). What intentionally has **no** Rust counterpart here:
//!
//! * Pi event hooks (`before_provider_request`, `message_end`, …) — feather
//!   calls [`CacheOptimizer`] through the `CachePlanner` seam instead.
//! * model-catalog auto-repair / rollback and the atomic receipt files — that
//!   is a host configuration concern, not a request concern.
//! * Footer stat shards under `pi-cache-optimizer-stats.d/` — feather keeps
//!   its own metrics.
//! * `Symbol.for("pi.routing.registry.v1")` / virtual-model routing — no
//!   dynamic extension host exists in feather.
//! * `PI_CACHE_RETENTION` environment mutation — feather models retention as
//!   [`CacheSettings::sticky_ttl_seconds`] plus the pure gate in [`plugin`].
//! * Opt-in deterministic tool ordering — feather's [`ToolRegistry`] already
//!   sorts tool definitions.
//!
//! [`ToolRegistry`]: feather_core::tool::ToolRegistry

pub mod plugin;

use feather_core::message::Usage;
use feather_core::provider::{CacheHints, ChatRequest};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;

/// Minimum new prefix growth (tokens) before a fresh message breakpoint is
/// worth writing. Conservative default matching the plugin's long-retention
/// behaviour for Anthropic-sized prompts.
pub const DEFAULT_MIN_BREAKPOINT_TOKENS: u64 = 1024;
/// Default ephemeral cache retention: Anthropic's 5-minute window. A value of
/// `Some(3600)` requests the 1h window when the endpoint supports it.
pub const DEFAULT_STICKY_TTL_SECONDS: u64 = 300;
pub const DEFAULT_CACHE_SYSTEM: bool = true;
pub const DEFAULT_CACHE_TOOLS: bool = true;

#[derive(Debug, Default)]
pub struct CacheMetrics {
    requests: AtomicU64,
    input_tokens: AtomicU64,
    cache_read: AtomicU64,
    cache_write: AtomicU64,
    output_tokens: AtomicU64,
    last_stable_prefix_len: AtomicU64,
    one_off: AtomicBool,
}

/// Serialized planner configuration. `None` for `sticky_ttl_seconds` means
/// "provider default (5m)"; `Some(3600)` asks for the 1h retention window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CacheSettings {
    pub min_breakpoint_tokens: u64,
    pub sticky_ttl_seconds: Option<u64>,
    pub cache_system: bool,
    pub cache_tools: bool,
}

impl Default for CacheSettings {
    fn default() -> Self {
        Self {
            min_breakpoint_tokens: DEFAULT_MIN_BREAKPOINT_TOKENS,
            sticky_ttl_seconds: Some(DEFAULT_STICKY_TTL_SECONDS),
            cache_system: DEFAULT_CACHE_SYSTEM,
            cache_tools: DEFAULT_CACHE_TOOLS,
        }
    }
}

/// Where the previous message-level breakpoint sits, and how many tokens the
/// stable prefix held there. Used to gate a *fresh* breakpoint on
/// `min_breakpoint_tokens`.
#[derive(Debug, Clone, Copy)]
struct BreakpointState {
    message_index: usize,
    prefix_tokens: u64,
}

pub struct CacheOptimizer {
    metrics: CacheMetrics,
    settings: CacheSettings,
    /// Per-message hashes recorded by the previous `plan` call.
    prev_hashes: Mutex<Vec<u64>>,
    /// Previous message breakpoint (index + prefix size).
    last_breakpoint: Mutex<Option<BreakpointState>>,
}

impl Default for CacheOptimizer {
    fn default() -> Self {
        Self::new()
    }
}

impl CacheOptimizer {
    /// Optimizer with the conservative compile-time defaults.
    pub fn new() -> Self {
        Self::with_settings(CacheSettings::default())
    }

    /// Optimizer with explicit settings (the builder the wiring layer uses).
    pub fn with_settings(settings: CacheSettings) -> Self {
        Self {
            metrics: CacheMetrics::default(),
            settings,
            prev_hashes: Mutex::new(Vec::new()),
            last_breakpoint: Mutex::new(None),
        }
    }

    pub fn settings(&self) -> CacheSettings {
        self.settings
    }

    pub fn set_settings(&mut self, settings: CacheSettings) {
        self.settings = settings;
    }

    /// Builder-style override of the minimum breakpoint growth.
    pub fn with_min_breakpoint_tokens(mut self, t: u64) -> Self {
        self.settings.min_breakpoint_tokens = t;
        self
    }

    /// Builder-style override of the retained TTL.
    pub fn with_sticky_ttl_seconds(mut self, ttl: Option<u64>) -> Self {
        self.settings.sticky_ttl_seconds = ttl;
        self
    }

    /// Longest message-prefix length identical to the previous request.
    pub fn stable_prefix_len(&self, new_hashes: &[u64], prev_hashes: &[u64]) -> usize {
        let mut i = 0;
        while i < new_hashes.len() && i < prev_hashes.len() && new_hashes[i] == prev_hashes[i] {
            i += 1;
        }
        i
    }

    pub fn snapshot_metrics(&self) -> CacheSnapshot {
        CacheSnapshot {
            requests: self.metrics.requests.load(Ordering::Relaxed),
            input_tokens: self.metrics.input_tokens.load(Ordering::Relaxed),
            cache_read: self.metrics.cache_read.load(Ordering::Relaxed),
            cache_write: self.metrics.cache_write.load(Ordering::Relaxed),
            output_tokens: self.metrics.output_tokens.load(Ordering::Relaxed),
            last_stable_prefix_len: self.metrics.last_stable_prefix_len.load(Ordering::Relaxed),
        }
    }

    pub fn hit_rate(&self) -> f64 {
        let m = self.snapshot_metrics();
        let served = m.input_tokens + m.cache_read;
        if served == 0 {
            0.0
        } else {
            m.cache_read as f64 / served as f64
        }
    }
}

#[derive(Debug, Clone, Copy, Default, serde::Serialize)]
pub struct CacheSnapshot {
    pub requests: u64,
    pub input_tokens: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub output_tokens: u64,
    pub last_stable_prefix_len: u64,
}

impl feather_core::agent::CachePlanner for CacheOptimizer {
    fn plan(&self, req: &mut ChatRequest, _history_hash: u64) {
        let hashes: Vec<_> = req
            .messages
            .iter()
            .map(|message| {
                feather_core::message::json_hash(message)
                    .expect("Message serialization is infallible")
            })
            .collect();
        let mut prefix = Vec::with_capacity(req.messages.len() + 1);
        let mut tokens = 16;
        prefix.push(tokens);
        for message in req.messages.iter() {
            tokens += feather_core::tokenizer::estimate_message_tokens(message);
            prefix.push(tokens);
        }
        self.plan_cached(req, _history_hash, &hashes, &prefix);
    }
    fn plan_cached(
        &self,
        req: &mut ChatRequest,
        _history_hash: u64,
        new_hashes: &[u64],
        prefix_tokens: &[u64],
    ) {
        let was_one_off = self.metrics.one_off.swap(false, Ordering::Relaxed);

        let stable = {
            let mut prev = self.prev_hashes.lock().unwrap_or_else(|e| e.into_inner());
            let s = self.stable_prefix_len(new_hashes, &prev);
            prev.clear();
            prev.extend_from_slice(new_hashes);
            s
        };
        let stable = if was_one_off { 0 } else { stable };
        self.metrics
            .last_stable_prefix_len
            .store(stable as u64, Ordering::Relaxed);

        if was_one_off {
            // one-off request (summary): never write cache entries
            req.cache = CacheHints::default();
            *self
                .last_breakpoint
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = None;
            return;
        }

        // Only advance the message breakpoint once the stable prefix has grown
        // by at least `min_breakpoint_tokens` since the last breakpoint;
        // otherwise keep reusing the previous one (the cached prefix still
        // matches) instead of paying for a new cache write each turn.
        let stable_prefix_tokens = if stable == 0 {
            0
        } else {
            prefix_tokens.get(stable).copied().unwrap_or_else(|| {
                feather_core::tokenizer::estimate_context_tokens(&req.messages[..stable])
            })
        };
        let stable_message_index = if stable == 0 {
            *self
                .last_breakpoint
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = None;
            None
        } else {
            let mut state = self
                .last_breakpoint
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let reuse = matches!(
                *state,
                Some(prev)
                    if prev.message_index < stable
                        && stable_prefix_tokens.saturating_sub(prev.prefix_tokens)
                            < self.settings.min_breakpoint_tokens
            );
            if reuse {
                state.map(|prev| prev.message_index)
            } else {
                let next = BreakpointState {
                    message_index: stable - 1,
                    prefix_tokens: stable_prefix_tokens,
                };
                *state = Some(next);
                Some(next.message_index)
            }
        };

        req.cache = CacheHints {
            system: self.settings.cache_system,
            tools: self.settings.cache_tools && !req.tools.is_empty(),
            stable_message_index,
            sticky_ttl_seconds: self.settings.sticky_ttl_seconds,
            min_breakpoint_tokens: self.settings.min_breakpoint_tokens,
        };
    }

    fn record(&self, usage: &Usage) {
        self.metrics.requests.fetch_add(1, Ordering::Relaxed);
        self.metrics
            .input_tokens
            .fetch_add(usage.input, Ordering::Relaxed);
        self.metrics
            .cache_read
            .fetch_add(usage.cache_read, Ordering::Relaxed);
        self.metrics
            .cache_write
            .fetch_add(usage.cache_write, Ordering::Relaxed);
        self.metrics
            .output_tokens
            .fetch_add(usage.output, Ordering::Relaxed);
    }

    fn demote_one_off(&self) {
        self.metrics.one_off.store(true, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn cached_planning_matches_uncached_after_append_and_replacement() {
        let cached = CacheOptimizer::new();
        let regular = CacheOptimizer::new();
        let mut session = feather_core::Session::memory(std::path::Path::new("fixture"), "planner");
        for n in 0..6 {
            session
                .push(Message::user_text(format!("{n} {}", "text ".repeat(1000))))
                .unwrap();
            let mut a = req_with(session.messages().to_vec());
            let mut b = a.clone();
            cached.plan_cached(
                &mut a,
                session.history_hash(),
                session.message_hashes(),
                session.prefix_tokens(),
            );
            regular.plan(&mut b, session.history_hash());
            assert_eq!(a.cache.stable_message_index, b.cache.stable_message_index);
            assert_eq!(
                cached.snapshot_metrics().last_stable_prefix_len,
                regular.snapshot_metrics().last_stable_prefix_len
            );
        }
        session.set_messages_for_compaction(vec![Message::user_text("summary")]);
        let mut a = req_with(session.messages().to_vec());
        let mut b = a.clone();
        cached.plan_cached(
            &mut a,
            session.history_hash(),
            session.message_hashes(),
            session.prefix_tokens(),
        );
        regular.plan(&mut b, session.history_hash());
        assert_eq!(a.cache.stable_message_index, b.cache.stable_message_index);
    }
    use super::*;
    use feather_core::agent::CachePlanner;
    use feather_core::message::Message;

    fn req_with(msgs: Vec<Message>) -> ChatRequest {
        ChatRequest {
            system: "sys".into(),
            messages: msgs.into(),
            tools: vec![feather_core::provider::ToolDef {
                name: "read".into(),
                description: "read".into(),
                parameters: serde_json::json!({}),
            }]
            .into(),
            model: "m".into(),
            max_tokens: 100,
            temperature: None,
            cache: CacheHints::default(),
            thinking: None,
            session_id: String::new(),
        }
    }

    #[test]
    fn stable_prefix_grows_with_append_only_history() {
        let opt = CacheOptimizer::new();
        let h = |m: &Message| {
            let b = serde_json::to_vec(m).unwrap();
            feather_core::message::fnv1a(&b)
        };
        let m1 = Message::user_text("one");
        let m2 = Message::user_text("two");
        let m3 = Message::user_text("three");
        let new1 = vec![h(&m1)];
        let new2 = vec![h(&m1), h(&m2)];
        let new3 = vec![h(&m1), h(&m2), h(&m3)];
        let prev: Vec<u64> = Vec::new();
        assert_eq!(opt.stable_prefix_len(&new1, &prev), 0);
        assert_eq!(opt.stable_prefix_len(&new2, &new1), 1);
        assert_eq!(opt.stable_prefix_len(&new3, &new2), 2);
        // a rewritten early message truncates the stable prefix
        let m1b = Message::user_text("one edited");
        let rewritten = vec![h(&m1b), h(&m2), h(&m3)];
        assert_eq!(opt.stable_prefix_len(&rewritten, &new3), 0);
    }

    #[test]
    fn plan_sets_breakpoints_then_demotes_one_off() {
        let opt = CacheOptimizer::new();
        let msgs = vec![Message::user_text("a"), Message::user_text("b")];
        let mut req = req_with(msgs.clone());
        let hash = 42;
        opt.plan(&mut req, hash);
        assert!(req.cache.system);
        // first request: no recorded prefix yet → system+tools breakpoints only
        assert!(req.cache.system);
        assert!(req.cache.tools);
        assert!(req.cache.stable_message_index.is_none());
        assert_eq!(
            req.cache.sticky_ttl_seconds,
            Some(DEFAULT_STICKY_TTL_SECONDS)
        );
        assert_eq!(
            req.cache.min_breakpoint_tokens,
            DEFAULT_MIN_BREAKPOINT_TOKENS
        );

        // second request with appended history: breakpoint at the stable prefix end
        let mut msgs2 = msgs.clone();
        msgs2.push(Message::user_text("c"));
        let mut req2 = req_with(msgs2.clone());
        opt.plan(&mut req2, hash);
        assert!(req2.cache.system);
        assert!(req2.cache.tools);
        assert_eq!(req2.cache.stable_message_index, Some(1));

        // summary request: demoted, no cache writes
        opt.demote_one_off();
        let mut req3 = req_with(msgs2.clone());
        opt.plan(&mut req3, hash);
        assert!(!req3.cache.system);
        assert!(!req3.cache.tools);
        assert!(req3.cache.stable_message_index.is_none());
    }

    #[test]
    fn settings_gate_fresh_breakpoints_and_are_serialized() {
        // a very high minimum keeps the first breakpoint pinned across small appends
        let opt = CacheOptimizer::with_settings(CacheSettings {
            min_breakpoint_tokens: 1_000_000,
            ..CacheSettings::default()
        });
        let mut msgs = vec![Message::user_text("a")];
        let mut req = req_with(msgs.clone());
        opt.plan(&mut req, 0);
        assert!(req.cache.stable_message_index.is_none());

        msgs.push(Message::user_text("b"));
        let mut req = req_with(msgs.clone());
        opt.plan(&mut req, 0);
        assert_eq!(req.cache.stable_message_index, Some(0));

        msgs.push(Message::user_text("c"));
        let mut req = req_with(msgs.clone());
        opt.plan(&mut req, 0);
        // not enough new prefix yet → reuse the existing breakpoint
        assert_eq!(req.cache.stable_message_index, Some(0));

        // zero minimum always advances
        let opt = CacheOptimizer::with_settings(CacheSettings {
            min_breakpoint_tokens: 0,
            ..CacheSettings::default()
        });
        let mut first = req_with(vec![Message::user_text("a")]);
        opt.plan(&mut first, 0);
        let mut second = req_with(vec![Message::user_text("a"), Message::user_text("b")]);
        opt.plan(&mut second, 0);
        assert_eq!(second.cache.stable_message_index, Some(0));
    }

    #[test]
    fn settings_round_trip_through_serde() {
        let settings = CacheSettings {
            min_breakpoint_tokens: 2048,
            sticky_ttl_seconds: Some(3600),
            cache_system: false,
            cache_tools: true,
        };
        let json = serde_json::to_string(&settings).unwrap();
        let back: CacheSettings = serde_json::from_str(&json).unwrap();
        assert_eq!(settings, back);
        assert_eq!(
            CacheSettings::default().sticky_ttl_seconds,
            Some(DEFAULT_STICKY_TTL_SECONDS)
        );
    }

    #[test]
    fn metrics_track_hit_rate() {
        let opt = CacheOptimizer::new();
        opt.record(&Usage {
            input: 100,
            output: 10,
            cache_read: 900,
            cache_write: 0,
        });
        opt.record(&Usage {
            input: 50,
            output: 5,
            cache_read: 950,
            cache_write: 0,
        });
        let snap = opt.snapshot_metrics();
        assert_eq!(snap.requests, 2);
        assert_eq!(snap.cache_read, 1850);
        assert!(opt.hit_rate() > 0.9);
    }
}
