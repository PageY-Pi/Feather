//! feather-bench — the ruler.
//!
//! One binary, one JSON document, no external crates beyond what the workspace
//! already builds. Every number it prints is either a wall-clock duration it
//! measured itself or the process's own peak working set — nothing here is
//! estimated, extrapolated, or "felt".
//!
//! ## Why a separate binary
//!
//! `cargo bench` needs `#[bench]` (nightly) or Criterion (a new dependency and a
//! per-crate target). This stays a plain binary so `cargo run -p feather-bench`
//! works on stable with no lockfile churn, and so the output can be diffed by a
//! shell script that knows nothing about Rust.
//!
//! ## Reading the output
//!
//! ```text
//! feather-bench [--iterations N] [--json PATH] [--only NAME[,NAME...]]
//! ```
//!
//! Each case reports:
//! * `ns_per_op` — median of `--iterations` timed batches, divided by the batch
//!   size. Median, not mean: one descheduled tick must not move the number.
//! * `peak_rss_kb` — peak working set *after* the case, so a case that leaks is
//!   visible. It is process-wide and monotonic, so the interesting signal is the
//!   delta between consecutive cases, not the absolute value.
//! * `correct` — the case's own invariant held. A perf win that breaks the
//!   invariant is not a win, and this ruler refuses to hide that in a separate
//!   test run that nobody re-runs mid-optimisation.

use std::time::Instant;

mod cases;
mod probe;
mod sys;

/// A single measured case.
pub struct Measurement {
    pub name: &'static str,
    /// Operations per timed batch — the denominator for `ns_per_op`.
    pub ops: u64,
    /// Median nanoseconds for one operation.
    pub ns_per_op: f64,
    /// Peak working set after the case, in KiB.
    pub peak_rss_kb: u64,
    /// The case's internal invariant held.
    pub correct: bool,
    /// Free-form, one-line detail (hit rate, bytes, counts). Empty when none.
    pub detail: String,
}

/// Run `batch` `iterations` times and report the median cost of one operation.
///
/// `batch` performs `ops` logical operations and returns something the caller
/// needs; the return value is dropped each iteration so the compiler cannot
/// elide the work, and `black_box` keeps the result observable.
fn measure(
    name: &'static str,
    ops: u64,
    iterations: usize,
    mut batch: impl FnMut() -> (bool, String),
) -> Measurement {
    let mut samples = Vec::with_capacity(iterations);
    let mut correct = true;
    let mut detail = String::new();
    for _ in 0..iterations {
        let start = Instant::now();
        let (ok, note) = batch();
        let elapsed = start.elapsed();
        samples.push(elapsed.as_secs_f64() * 1e9 / ops.max(1) as f64);
        correct &= ok;
        if !note.is_empty() {
            detail = note;
        }
    }
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let median = samples.get(samples.len() / 2).copied().unwrap_or(f64::NAN);
    Measurement {
        name,
        ops,
        ns_per_op: median,
        peak_rss_kb: sys::peak_rss_kb(),
        correct,
        detail,
    }
}

fn main() {
    let mut iterations = 9usize;
    let mut json_path: Option<String> = None;
    let mut only: Option<Vec<String>> = None;

    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--iterations" => {
                iterations = args
                    .get(i + 1)
                    .and_then(|v| v.parse().ok())
                    .filter(|n| *n > 0)
                    .unwrap_or_else(|| {
                        eprintln!("feather-bench: --iterations needs a positive integer");
                        std::process::exit(2);
                    });
                i += 2;
            }
            "--json" => {
                json_path = args.get(i + 1).cloned();
                if json_path.is_none() {
                    eprintln!("feather-bench: --json needs a path");
                    std::process::exit(2);
                }
                i += 2;
            }
            "--only" => {
                let raw = args.get(i + 1).cloned().unwrap_or_default();
                only = Some(raw.split(',').map(|s| s.trim().to_string()).collect());
                i += 2;
            }
            // Diagnostic mode: attribute `commit_turn`'s cost to sub-steps.
            // Deliberately not a registry case — isolated steps do not sum to the
            // whole, and the difference is the point.
            "--probe" => {
                let scratch = std::env::temp_dir()
                    .join(format!("feather-bench-probe-{}", std::process::id()));
                let _ = std::fs::create_dir_all(&scratch);
                probe::probe(&scratch);
                let _ = std::fs::remove_dir_all(&scratch);
                return;
            }
            "--help" | "-h" => {
                println!(
                    "feather-bench — performance ruler\n\n\
                     USAGE: feather-bench [--iterations N] [--json PATH] [--only NAME[,NAME...]]\n\n\
                     Emits one JSON document to stdout (or PATH) with per-case ns/op,\n\
                     peak RSS and a correctness flag. Compare two runs with scripts/bench.ps1."
                );
                return;
            }
            other => {
                eprintln!("feather-bench: unknown argument {other:?} (try --help)");
                std::process::exit(2);
            }
        }
    }

    let selected = |name: &str| match &only {
        None => true,
        Some(names) => names
            .iter()
            .any(|n| n == name || name.starts_with(&format!("{n}."))),
    };

    let scratch = std::env::temp_dir().join(format!("feather-bench-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&scratch);

    let mut results: Vec<Measurement> = Vec::new();
    for case in cases::registry() {
        if !selected(case.name) {
            continue;
        }
        let m = (case.run)(&scratch, iterations);
        println!(
            "{:<28} {:>12.1} ns/op  {:>8} KiB  {}{}",
            m.name,
            m.ns_per_op,
            m.peak_rss_kb,
            if m.correct { "ok" } else { "FAILED" },
            if m.detail.is_empty() {
                String::new()
            } else {
                format!("  {}", m.detail)
            }
        );
        results.push(m);
    }

    let _ = std::fs::remove_dir_all(&scratch);

    let payload = serde_json::json!({
        "version": env!("CARGO_PKG_VERSION"),
        "iterations": iterations,
        "cases": results.iter().map(|m| serde_json::json!({
            "name": m.name,
            "ops": m.ops,
            "ns_per_op": m.ns_per_op,
            "peak_rss_kb": m.peak_rss_kb,
            "correct": m.correct,
            "detail": m.detail,
        })).collect::<Vec<_>>(),
    });
    let text = serde_json::to_string_pretty(&payload).expect("bench payload is serializable");
    match json_path {
        Some(path) => {
            if let Some(parent) = std::path::Path::new(&path).parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Err(e) = std::fs::write(&path, &text) {
                eprintln!("feather-bench: cannot write {path}: {e}");
                std::process::exit(1);
            }
            println!("\nwrote {path}");
        }
        None => println!("\n{text}"),
    }

    if results.iter().any(|m| !m.correct) {
        eprintln!("\nfeather-bench: at least one case reported FAILED (correctness invariant)");
        std::process::exit(1);
    }
}
