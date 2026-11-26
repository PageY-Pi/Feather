//! Verify `search_context` against real sessions on this machine.
//!
//! Unit tests use synthetic audits, which proves the logic but not that the tool
//! works on data the product actually produced. This walks the real session
//! stores under `~/.feather/sessions`, runs the same search function the tool
//! calls, and reports what it found — plus the cost, so a pathological session
//! cannot hide behind a good average.
//!
//! Usage: cargo run --release --example search_context_smoke [query]

use feather_core::ctxsearch::{search_session, RoleFilter};
use std::time::Instant;

fn main() {
    let query = std::env::args().nth(1).unwrap_or_else(|| "fn ".to_string());
    // Resolve the feather home the same way the binary does: FEATHER_HOME wins,
    // otherwise ~/.feather.
    let home = match std::env::var("FEATHER_HOME") {
        Ok(h) if !h.is_empty() => std::path::PathBuf::from(h),
        _ => {
            let user = std::env::var("USERPROFILE")
                .or_else(|_| std::env::var("HOME"))
                .unwrap_or_default();
            std::path::PathBuf::from(user).join(".feather")
        }
    };
    let sessions = home.join("sessions");
    let mut dirs: Vec<_> = match std::fs::read_dir(&sessions) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.is_dir() && p.join("session.redb").is_file())
            .collect(),
        Err(e) => {
            eprintln!("no sessions dir at {}: {e}", sessions.display());
            std::process::exit(1);
        }
    };
    dirs.sort();
    println!("query {query:?} over {} real sessions\n", dirs.len());

    let mut total_hits = 0usize;
    let mut worst = 0f64;
    let mut failures = 0usize;
    for dir in &dirs {
        let started = Instant::now();
        let result = search_session(dir, &query, RoleFilter::Any, 5, 2000);
        let elapsed = started.elapsed().as_secs_f64() * 1e3;
        worst = worst.max(elapsed);
        match result {
            Ok(found) => {
                total_hits += found.hits.len();
                let name = dir.file_name().unwrap_or_default().to_string_lossy();
                println!(
                    "{name}: {elapsed:7.2} ms  hits={} scanned={} total={} more={}",
                    found.hits.len(),
                    found.scanned,
                    found.total,
                    found.more_may_exist
                );
                for hit in found.hits.iter().take(1) {
                    let snippet: String = hit.snippet.chars().take(100).collect();
                    println!("    seq {} [{}] {}", hit.seq, hit.role, snippet);
                }
            }
            Err(e) => {
                failures += 1;
                println!("{}: ERROR {e}", dir.display());
            }
        }
    }
    println!(
        "\n{total_hits} hits across {} sessions, worst {worst:.2} ms, {failures} failures",
        dirs.len()
    );
    if failures > 0 {
        std::process::exit(1);
    }
}
