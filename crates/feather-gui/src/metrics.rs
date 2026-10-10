//! Pure, allocation-light streaming throughput metrics for the egui app.
//!
//! Every timestamp is supplied by the caller as monotonic seconds (`f64`) —
//! typically `egui::Context::input(|i| i.time)` — so behaviour is deterministic
//! and unit-testable without wall-clock timers. No `egui`, `std::time`, or I/O
//! types leak in here; the parent crate owns integration and rendering.
//!
//! Lifecycle:
//! ```text
//! begin_run(now)
//!   begin_turn(now)
//!     record_delta(text, now) ...    // plain text and/or thinking chunks
//!   finish_turn(output_tokens, now)  // final Usage output-token count
//!   ...                              // tool waits happen between turns
//! finish_run(now)
//! ```
//!
//! Live estimates convert streamed characters to tokens with the usual
//! 4-chars-per-token rule over a rolling [`ROLLING_WINDOW`]-second window and
//! are rendered as `~X tok/s`, so an estimate can never be mistaken for a real
//! number. Final rates use the provider's `Usage` output-token count over the
//! measured generation window (first streamed delta → turn end, floored at
//! [`MIN_GENERATION_SECONDS`]) and drop the `~`.
//!
//! Deliberately excluded from measurement:
//! * request setup before the first streamed chunk (the clock starts on the
//!   first non-empty [`StreamingMetrics::record_delta`]);
//! * tool waits between turns (a finished turn is frozen, so a later `now`
//!   cannot make the last rate decay while nothing is generating);
//! * UTF-8 byte counts — only Unicode scalar values are counted.
//!
//! No final ("actual") tok/s is ever fabricated: a turn that streamed nothing,
//! or whose provider reported no output tokens, reports [`None`] rather than a
//! made-up number.

#![allow(dead_code)]

use std::collections::VecDeque;

/// Characters per token used by the live estimate.
pub const CHARS_PER_TOKEN: f64 = 4.0;

/// Rolling window (seconds) backing the live estimate.
pub const ROLLING_WINDOW: f64 = 2.0;

/// Measured generation windows shorter than this are floored to it, so a very
/// fast turn can never divide by (near) zero.
pub const MIN_GENERATION_SECONDS: f64 = 0.1;

/// Hard cap on remembered delta events, independent of the time window, so a
/// pathological burst cannot grow memory without bound.
pub const MAX_DELTA_EVENTS: usize = 512;

/// A single streamed chunk reduced to its char count.
#[derive(Debug, Clone, Copy)]
struct Delta {
    at: f64,
    chars: u32,
}

/// Streaming token-throughput tracker.
///
/// All methods are cheap and side-effect free apart from the explicit
/// `begin_*` / `record_*` / `finish_*` mutators. Readers ([`Self::rate`],
/// [`Self::label`], [`Self::is_estimated`]) take `&self` and a caller-supplied
/// `now`, so the parent can query every frame without mutating state.
#[derive(Debug, Clone, Default)]
pub struct StreamingMetrics {
    /// A turn is open between `begin_turn` and `finish_turn`.
    turn_open: bool,
    /// Timestamp of the first streamed chunk of the current turn. `None` means
    /// nothing has streamed yet (setup excluded).
    first_delta_at: Option<f64>,
    /// Bounded history of char counts for the rolling window.
    deltas: VecDeque<Delta>,
    /// Final output tokens reported by `Usage` for the last finished turn.
    last_output_tokens: u64,
    /// Rate captured when the last turn finished. `None` until then, and after
    /// a turn that produced no measurable generation.
    frozen_rate: Option<f64>,
    /// Whether `frozen_rate` is an estimate (no `Usage`) rather than accurate.
    frozen_is_estimate: bool,
    /// Run scope, grouping turns so tool waits are excluded from the aggregate.
    run_open: bool,
    run_started_at: Option<f64>,
    run_finished_at: Option<f64>,
    /// Output tokens summed over turns that had a measured generation window.
    run_output_tokens: u64,
    /// Sum of measured generation windows (first delta → finish) across turns.
    run_generation_seconds: f64,
}

impl StreamingMetrics {
    /// Fresh, idle tracker.
    pub fn new() -> Self {
        Self::default()
    }

    /// Start a turn. Clears any previous turn's deltas and frozen rate so an
    /// old TPS can never leak into the new turn.
    pub fn begin_turn(&mut self, _now: f64) {
        self.turn_open = true;
        self.first_delta_at = None;
        self.deltas.clear();
        self.last_output_tokens = 0;
        self.frozen_rate = None;
        self.frozen_is_estimate = false;
    }

    /// Record a streamed chunk (assistant text *or* thinking). `text` is
    /// counted in Unicode scalar values, never UTF-8 bytes. Empty chunks are
    /// ignored and do not start the generation clock.
    ///
    /// The first non-empty chunk of a turn starts the measured window; request
    /// setup before it is therefore excluded.
    pub fn record_delta(&mut self, text: &str, now: f64) {
        let chars = text.chars().count();
        if chars == 0 {
            return;
        }
        // Tolerate a missing begin_turn by opening the turn implicitly.
        if !self.turn_open {
            self.begin_turn(now);
        }
        if self.first_delta_at.is_none() {
            self.first_delta_at = Some(now);
            self.frozen_rate = None;
            self.frozen_is_estimate = false;
        }
        self.deltas.push_back(Delta {
            at: now,
            chars: chars.min(u32::MAX as usize) as u32,
        });
        self.prune(now);
    }

    /// Finish the current turn with the provider's final `Usage` output-token
    /// count and the turn's end timestamp.
    ///
    /// The final rate is `output_tokens / max(now - first_delta, 0.1)`. It is
    /// only produced when the turn actually streamed *and* `output_tokens > 0`;
    /// otherwise the last live estimate is frozen (still marked estimated) or
    /// nothing is reported at all. No actual TPS is fabricated.
    pub fn finish_turn(&mut self, output_tokens: u64, now: f64) {
        if !self.turn_open {
            return;
        }
        self.turn_open = false;
        self.last_output_tokens = output_tokens;

        let Some(first) = self.first_delta_at else {
            // Nothing streamed: no measured generation window exists.
            self.frozen_rate = None;
            self.frozen_is_estimate = false;
            return;
        };

        let elapsed = (now - first).max(MIN_GENERATION_SECONDS);
        if output_tokens > 0 {
            self.frozen_rate = Some(output_tokens as f64 / elapsed);
            self.frozen_is_estimate = false;
            self.run_output_tokens = self.run_output_tokens.saturating_add(output_tokens);
            self.run_generation_seconds += elapsed;
        } else {
            // Usage missing/empty: keep the clearly-marked live estimate, and
            // freeze it so a following tool wait cannot make it decay.
            self.frozen_rate = self.roll(now);
            self.frozen_is_estimate = self.frozen_rate.is_some();
        }
    }

    /// Open a run grouping one or more turns. Resets the run aggregate.
    pub fn begin_run(&mut self, now: f64) {
        self.run_open = true;
        self.run_started_at = Some(now);
        self.run_finished_at = None;
        self.run_output_tokens = 0;
        self.run_generation_seconds = 0.0;
    }

    /// Close the run, freezing its elapsed time.
    pub fn finish_run(&mut self, now: f64) {
        self.run_open = false;
        self.run_finished_at = Some(now);
    }

    /// Final output tokens of the last finished turn (from `Usage`).
    pub fn output_tokens(&self) -> u64 {
        self.last_output_tokens
    }

    /// Alias for [`Self::output_tokens`].
    pub fn last_output_tokens(&self) -> u64 {
        self.last_output_tokens
    }

    /// Whether the value returned by [`Self::rate`] is an estimate.
    ///
    /// `true` while a turn is live after its first delta, or when the finished
    /// turn had to fall back to the live estimate; `false` for an accurate
    /// `Usage`-based rate or when there is nothing to report.
    pub fn is_estimated(&self) -> bool {
        if self.frozen_rate.is_some() {
            self.frozen_is_estimate
        } else {
            self.first_delta_at.is_some()
        }
    }

    /// Whether any chunk has streamed for the current turn.
    pub fn has_streamed(&self) -> bool {
        self.first_delta_at.is_some()
    }

    /// Current tokens/second, or `None` when there is nothing measurable.
    ///
    /// Returns the frozen final value after a finished turn, otherwise the live
    /// rolling estimate for an open turn.
    pub fn rate(&self, now: f64) -> Option<f64> {
        if let Some(rate) = self.frozen_rate {
            return Some(rate);
        }
        if self.turn_open && self.first_delta_at.is_some() {
            return self.roll(now);
        }
        None
    }

    /// Display string for the current rate, e.g. `~12.3 tok/s` for a live
    /// estimate or `12.3 tok/s` for a final accurate one. Empty when there is
    /// nothing to show.
    pub fn label(&self, now: f64) -> String {
        if let Some(rate) = self.rate(now) {
            let rate = if rate.is_finite() { rate.max(0.0) } else { 0.0 };
            if self.is_estimated() {
                format!("~{rate:.1} tok/s")
            } else {
                format!("{rate:.1} tok/s")
            }
        } else if self.first_delta_at.is_some() {
            // Streaming but the window is still zero-length.
            "~0.0 tok/s".to_string()
        } else {
            String::new()
        }
    }

    /// Aggregate tokens/second over a run, using only turns that had both a
    /// measured generation window and a non-zero `Usage` count.
    pub fn run_rate(&self) -> Option<f64> {
        if self.run_output_tokens > 0 && self.run_generation_seconds > 0.0 {
            Some(self.run_output_tokens as f64 / self.run_generation_seconds)
        } else {
            None
        }
    }

    /// Output tokens accumulated across measured turns of the run.
    pub fn run_output_tokens(&self) -> u64 {
        self.run_output_tokens
    }

    /// Measured generation seconds accumulated across the run.
    pub fn run_generation_seconds(&self) -> f64 {
        self.run_generation_seconds
    }

    /// Wall-clock seconds of the run (frozen at `finish_run`).
    pub fn run_elapsed(&self, now: f64) -> Option<f64> {
        let start = self.run_started_at?;
        let end = self.run_finished_at.unwrap_or(now);
        Some((end - start).max(0.0))
    }

    /// Live rolling estimate: chars streamed inside the last
    /// [`ROLLING_WINDOW`] seconds (but never before the first delta), divided
    /// by [`CHARS_PER_TOKEN`] and by the window actually covered.
    ///
    /// Because the denominator keeps growing toward the full window, a stalled
    /// stream decays smoothly to `0.0` instead of sticking at its last value.
    fn roll(&self, now: f64) -> Option<f64> {
        let first = self.first_delta_at?;
        let window_start = (now - ROLLING_WINDOW).max(first);
        let span = now - window_start;
        if span <= 0.0 {
            return None;
        }
        let chars: usize = self
            .deltas
            .iter()
            .filter(|d| d.at >= window_start && d.at <= now)
            .map(|d| d.chars as usize)
            .sum();
        Some((chars as f64 / CHARS_PER_TOKEN) / span)
    }

    /// Drop events that fell out of the rolling window, then enforce the hard
    /// event cap.
    fn prune(&mut self, now: f64) {
        while let Some(front) = self.deltas.front() {
            if now - front.at > ROLLING_WINDOW {
                self.deltas.pop_front();
            } else {
                break;
            }
        }
        while self.deltas.len() > MAX_DELTA_EVENTS {
            self.deltas.pop_front();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-9, "expected {b}, got {a}");
    }

    #[test]
    fn first_delta_starts_clock_and_marks_estimated() {
        let mut m = StreamingMetrics::default();
        m.begin_turn(0.0);

        // Before any chunk there is nothing to report, and setup is excluded.
        assert_eq!(m.rate(0.0), None);
        assert!(!m.is_estimated());
        assert_eq!(m.label(0.0), "");

        m.record_delta("abcd", 0.0);
        assert!(m.has_streamed());
        assert!(m.is_estimated());
        // Zero-length window: still clearly marked as an estimate.
        assert_eq!(m.rate(0.0), None);
        assert_eq!(m.label(0.0), "~0.0 tok/s");

        // One token over 1s.
        close(m.rate(1.0).unwrap(), 1.0);
        assert_eq!(m.label(1.0), "~1.0 tok/s");
    }

    #[test]
    fn no_delta_never_fabricates_actual_rate() {
        let mut m = StreamingMetrics::default();
        m.begin_turn(0.0);
        m.finish_turn(0, 1.0);

        assert_eq!(m.rate(1.0), None);
        assert!(!m.is_estimated());
        assert_eq!(m.label(1.0), "");
        assert_eq!(m.output_tokens(), 0);

        // Even a Usage count with no streamed output yields no measured window.
        let mut m = StreamingMetrics::default();
        m.begin_turn(0.0);
        m.finish_turn(100, 1.0);
        assert_eq!(m.output_tokens(), 100);
        assert_eq!(m.rate(1.0), None);
        assert_eq!(m.label(1.0), "");
        assert!(!m.is_estimated());
    }

    #[test]
    fn rolling_window_averages_then_decays_on_stall() {
        let mut m = StreamingMetrics::default();
        m.begin_turn(0.0);
        // 40 chars == 10 tokens at t=0, another 40 at t=3.
        m.record_delta("x".repeat(40).as_str(), 0.0);
        m.record_delta("y".repeat(40).as_str(), 3.0);

        // At t=4 only the last-2s window counts: window_start = 2.0, so the
        // t=0 event is excluded -> 10 tokens / 2s = 5.0.
        close(m.rate(4.0).unwrap(), 5.0);

        // Stall past the window -> 0.0, not the last value.
        close(m.rate(10.0).unwrap(), 0.0);
        assert_eq!(m.label(10.0), "~0.0 tok/s");
    }

    #[test]
    fn final_rate_uses_usage_over_measured_window() {
        let mut m = StreamingMetrics::default();
        m.begin_turn(0.0);
        m.record_delta("hello", 0.25); // clock starts at 0.25, ignored setup before
        m.finish_turn(20, 2.25);

        // 20 tokens / (2.25 - 0.25) == 10.0, and it is accurate (no `~`).
        close(m.rate(99.0).unwrap(), 10.0);
        assert!(!m.is_estimated());
        assert_eq!(m.output_tokens(), 20);
        assert_eq!(m.label(99.0), "10.0 tok/s");

        // Sub-minimum windows are floored at MIN_GENERATION_SECONDS.
        let mut m = StreamingMetrics::default();
        m.begin_turn(0.0);
        m.record_delta("abcd", 0.0);
        m.finish_turn(2, 0.0);
        close(m.rate(5.0).unwrap(), 2.0 / MIN_GENERATION_SECONDS);
    }

    #[test]
    fn missing_usage_freezes_the_estimate() {
        let mut m = StreamingMetrics::default();
        m.begin_turn(0.0);
        m.record_delta("abcd", 0.0);
        m.finish_turn(0, 1.0);

        assert!(m.is_estimated());
        close(m.rate(1.0).unwrap(), 1.0);
        // Frozen: a later query (e.g. during a tool wait) does not decay it.
        close(m.rate(50.0).unwrap(), 1.0);
        assert_eq!(m.label(50.0), "~1.0 tok/s");
        assert_eq!(m.output_tokens(), 0);
    }

    #[test]
    fn begin_turn_resets_previous_tps() {
        let mut m = StreamingMetrics::default();
        m.begin_turn(0.0);
        m.record_delta("abcd", 0.0);
        assert!(m.rate(1.0).is_some());

        m.begin_turn(10.0);
        assert_eq!(m.rate(10.0), None);
        assert_eq!(m.label(10.0), "");
        assert!(!m.is_estimated());
        assert_eq!(m.output_tokens(), 0);
        assert!(!m.has_streamed());
    }

    #[test]
    fn unicode_counts_chars_not_utf8_bytes() {
        let mut m = StreamingMetrics::default();
        m.begin_turn(0.0);
        // 3 scalar values / 9 UTF-8 bytes; a byte count would give 2.25 tok/s.
        m.record_delta("日本語", 0.0);
        close(m.rate(1.0).unwrap(), 0.75);
        assert_eq!(m.label(1.0), "~0.8 tok/s");

        // Final path agrees: 3 chars -> 3/4 token, reported via Usage.
        let mut m = StreamingMetrics::default();
        m.begin_turn(0.0);
        m.record_delta("😀😀😀😀", 0.0); // 4 scalar values, 16 bytes
        m.finish_turn(1, 1.0);
        close(m.rate(1.0).unwrap(), 1.0);
    }

    #[test]
    fn empty_deltas_are_ignored() {
        let mut m = StreamingMetrics::default();
        m.begin_turn(0.0);
        m.record_delta("", 0.0);
        assert!(!m.has_streamed());
        assert_eq!(m.rate(1.0), None);
    }

    #[test]
    fn run_aggregate_excludes_tool_waits_between_turns() {
        let mut m = StreamingMetrics::default();
        m.begin_run(0.0);

        m.begin_turn(0.0);
        m.record_delta("abcd", 0.0);
        m.finish_turn(2, 1.0); // 2 tokens / 1s

        // Long tool wait: no turn open, nothing accumulates.
        m.begin_turn(100.0);
        m.record_delta("efgh", 100.0);
        m.finish_turn(3, 101.0); // 3 tokens / 1s
        m.finish_run(101.0);

        close(m.run_rate().unwrap(), 2.5);
        assert_eq!(m.run_output_tokens(), 5);
        close(m.run_generation_seconds(), 2.0);
        close(m.run_elapsed(999.0).unwrap(), 101.0);

        // A fresh run clears the aggregate.
        m.begin_run(200.0);
        assert_eq!(m.run_rate(), None);
    }

    #[test]
    fn history_is_bounded() {
        let mut m = StreamingMetrics::default();
        m.begin_turn(0.0);
        for i in 0..(MAX_DELTA_EVENTS * 2) {
            m.record_delta("abc", i as f64 * 0.001);
        }
        assert!(m.deltas.len() <= MAX_DELTA_EVENTS);
    }
}
