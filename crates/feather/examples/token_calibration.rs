//! How far off is the token estimate that drives compaction?
//!
//! `tokenizer::estimate_tokens` is a chars/4 heuristic because feather ships no
//! BPE tables. That estimate is what `compaction::should_compact_request`
//! compares against the model's context window, so its error decides two real
//! outcomes: compacting far too early wastes a summary and loses detail, and
//! compacting too late sends an over-limit request that the provider rejects.
//!
//! Every assistant message records the provider's own `Usage`, so the error is
//! measurable on real sessions rather than argued about. For each assistant turn
//! this compares the estimate of the messages *up to that point* against the
//! `input + cache_read + cache_write` the provider actually billed.
//!
//! Run: `cargo run --release -p feather --example token_calibration -- [session-dir]`
//!
//! # What the measurement found (24 sessions)
//!
//! The first billed turn of a session is the only unambiguous calibration point,
//! because compaction cannot have removed anything the audit keeps. Across 24
//! sessions its ratio ran from **-20.4% to +29.3%**, median about **-9%**: the
//! heuristic is wrong in *both* directions, and by an amount that depends on the
//! content (CJK text, code, and JSON schemas tokenize very differently from prose).
//!
//! Two conclusions follow, and they are why `agent::EstimateBias` exists:
//!
//! - A fixed safety margin has no correct value. Any constant large enough to cover
//!   the -20% cases pushes the +29% cases 50% past their window, compacting early
//!   and wasting a summary. That is a two-sided error and a one-sided remedy.
//! - The ratio is learnable per session from the `Usage` every response carries, so
//!   the correction can be measured instead of guessed. The agent folds each bill
//!   into a bounded, smoothed multiplier used only at the compaction threshold.
//!
//! Superseded measurements, kept so they are not repeated: comparing the *messages*
//! alone against the bill appears to be -95% low, which is entirely the static part
//! (system prompt + schemas) that this probe originally omitted; and consecutive-turn
//! *growth* ratios (median 1.0-1.7 with p25 down to 0.83) are too noisy to calibrate
//! against, because one turn's tool result dominates the delta.

use feather_core::session_store::SessionStore;

fn main() -> anyhow::Result<()> {
    let dir = match std::env::args().nth(1) {
        Some(d) => std::path::PathBuf::from(d),
        None => {
            // Newest session in the default home, which is the one most likely to
            // hold a long conversation worth calibrating against.
            let home = feather::config::feather_home();
            let sessions = home.join("sessions");
            let mut found: Vec<_> = std::fs::read_dir(&sessions)?
                .flatten()
                .filter(|e| e.path().is_dir())
                .collect();
            found.sort_by_key(|e| e.metadata().and_then(|m| m.modified()).ok());
            let last = found
                .pop()
                .ok_or_else(|| anyhow::anyhow!("no sessions under {}", sessions.display()))?;
            last.path()
        }
    };
    let store = SessionStore::open_existing(&dir)?
        .ok_or_else(|| anyhow::anyhow!("no session at {}", dir.display()))?;
    let entries = store.raw_entries()?;
    // The static half of every request, exactly as the agent computes it. The tool
    // schemas are the live registry's, not a copy: they are the largest stable part
    // of the bill and the one most likely to drift after a description edit.
    let tools = feather::wiring::tool_defs_for_estimate();
    // The *full* prompt, not the base instructions: skills metadata (~800 tokens)
    // and project context are part of every request and were missing from an
    // earlier version of this measurement.
    let home = feather::config::feather_home();
    let system = feather::wiring::full_system_prompt(&dir, &home);
    let static_tokens = feather_core::tokenizer::estimate_request_static_tokens(&system, &tools);
    println!(
        "static request part: ~{static_tokens} tokens ({} tools)",
        tools.len()
    );
    println!("session {} — {} audit rows", dir.display(), entries.len());

    // Walk the audit in order, tracking the running message list so each
    // assistant turn can be compared against the context that produced it.
    let mut context: Vec<feather_core::message::Message> = Vec::new();
    let mut rows: Vec<(u64, u64, u64)> = Vec::new(); // (seq, estimate, actual)
    for entry in &entries {
        let msg = &entry.message;
        // Compaction does not rewrite the audit: the originals stay and a summary
        // row is appended. Replaying the audit therefore sees turns the model never
        // received. A summary row is the boundary — everything before it is no longer
        // in the context the provider was sent — so the running list restarts there.
        // Without this the estimate measured 5.8x the actual, which is a bug in the
        // probe, not in the tokenizer.
        if msg.tool_call_id.is_none() && msg.text_cow().starts_with("## Conversation summary") {
            context.clear();
        }
        context.push(msg.clone());
        let Some(usage) = msg.usage.as_ref() else {
            continue;
        };
        // Only assistant turns carry usage; the billed input is everything sent
        // for that turn, which is the whole context before its own output.
        let sent = usage.input + usage.cache_read + usage.cache_write;
        if sent == 0 {
            continue;
        }
        // Estimate the same slice the provider saw: the context up to but not
        // including this assistant message.
        let upto = &context[..context.len() - 1];
        // The provider bills the *whole* request: system prompt, every tool
        // schema, then the messages. Measuring only the messages understated it by
        // ~3 400 tokens — the same as the static part — which looked like the
        // estimate running 95% low. It was the probe that was wrong.
        let estimate = static_tokens + feather_core::tokenizer::estimate_context_tokens(upto);
        rows.push((entry.seq, estimate, sent));
    }
    if rows.is_empty() {
        println!("no assistant turns with recorded usage — nothing to calibrate");
        return Ok(());
    }

    // Report the ratio distribution, since a systematic bias is what matters:
    // an estimate that is consistently 30% low makes every threshold wrong by 30%.
    let mut ratios: Vec<f64> = rows
        .iter()
        .map(|(_, est, act)| *est as f64 / *act as f64)
        .collect();
    ratios.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    // The first billed turn of a session is the only unambiguous calibration
    // point: the request was exactly the system prompt, the tool schemas and one
    // user message, so no compaction can have removed anything the audit keeps.
    // Later turns are still reported, but they cannot distinguish "the estimate is
    // low" from "the audit remembers messages the provider never saw".
    // Between two consecutive settled turns the static part is identical, so the
    // *growth* of the estimate and the growth of the bill differ only by what the
    // message estimator got wrong. This isolates the estimator from the system
    // prompt, which the audit does not record and this probe therefore cannot
    // reproduce.
    let mut growth: Vec<f64> = Vec::new();
    for pair in rows.windows(2) {
        let (_, est_a, act_a) = pair[0];
        let (_, est_b, act_b) = pair[1];
        // Growth must be positive: a turn whose bill shrank means compaction moved
        // the context, and that pair says nothing about the estimator.
        if act_b > act_a && est_b > est_a {
            growth.push((est_b - est_a) as f64 / (act_b - act_a) as f64);
        }
    }
    if !growth.is_empty() {
        growth.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let gp = |p: f64| growth[((growth.len() - 1) as f64 * p) as usize];
        println!(
            "message-estimator growth over {} clean step(s): p25 {:.3}  median {:.3}  p75 {:.3}",
            growth.len(),
            gp(0.25),
            gp(0.5),
            gp(0.75)
        );
    }
    let first = rows.first().copied();
    if let Some((seq, est, act)) = first {
        println!(
            "first turn (clean measurement): seq {seq} — estimate {est} vs actual {act} ({:+.1}%)",
            (est as f64 - act as f64) / act as f64 * 100.0
        );
    }
    let pct = |p: f64| ratios[((ratios.len() - 1) as f64 * p) as usize];
    println!(
        "assistant turns with usage: {}\n\
         estimate/actual:  min {:.3}  p25 {:.3}  median {:.3}  p75 {:.3}  max {:.3}",
        rows.len(),
        ratios[0],
        pct(0.25),
        pct(0.5),
        pct(0.75),
        ratios[ratios.len() - 1]
    );

    // The worst case is the one that breaks a request: a turn that is actually
    // over the window while the estimate still says it fits.
    let worst = rows
        .iter()
        .min_by_key(|(_, _, act)| std::cmp::Reverse(*act))
        .expect("rows non-empty");
    println!(
        "largest billed turn: seq {} — estimate {} vs actual {} ({:+.1}%)",
        worst.0,
        worst.1,
        worst.2,
        (worst.1 as f64 - worst.2 as f64) / worst.2 as f64 * 100.0
    );
    let under = rows.iter().filter(|(_, e, a)| e < a).count();
    println!(
        "turns where the estimate UNDER-counts (the dangerous direction): {under}/{}",
        rows.len()
    );
    Ok(())
}
