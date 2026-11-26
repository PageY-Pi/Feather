//! Where does `Session::commit_turn`'s ~28 ms actually go?
//!
//! The bench measures the whole call, which cannot distinguish a durable fsync from
//! a scan from a git spawn — and those have completely different fixes. This runs the
//! same work the way `commit_turn` does, timing each stage in isolation against a
//! session of a stated size, so an optimisation can be justified before it is written.
//!
//! Run: `cargo run --release -p feather --example commit_stages`

use feather_core::compaction::file_ops;
use feather_core::message::Message;
use feather_core::session::Session;
use std::path::Path;
use std::time::Instant;

fn history(n: usize) -> Vec<Message> {
    let mut out = Vec::new();
    for i in 0..n {
        out.push(Message::user_text(format!(
            "request {i} {}",
            "context ".repeat(20)
        )));
        // A tool call as the model would emit it: this is what `file_ops` scans for.
        out.push(Message::tool_result(
            format!("call-{i}"),
            "read",
            format!("file contents {i} {}", "line ".repeat(40)),
            false,
        ));
    }
    out
}

fn ms(d: std::time::Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = std::env::temp_dir().join(format!("feather-stages-{}", std::process::id()));
    let home = root.join("home");
    let cwd = root.join("cwd");
    std::fs::create_dir_all(&home)?;
    std::fs::create_dir_all(&cwd)?;
    let mut session = Session::new(&home, &cwd)?;
    for m in history(60) {
        session.push(m)?;
    }

    // Stage: `file_ops` alone, over the live history.
    let messages = session.messages().to_vec();
    let t = Instant::now();
    let (reads, writes) = file_ops(&messages);
    let file_ops_ms = ms(t.elapsed());

    // Stage: the tracked-path read. Measured twice on the sample instance: the first
    // call pays redb startup, the second answers from the per-instance handle cache.
    // The gap between them is what `Session` now keeps alive across turns.
    let fs = feather_core::file_snapshots::FileSnapshots::new(session.dir());
    let t = Instant::now();
    let owned = fs.tracked_paths().unwrap_or_default();
    let tracked_cold_ms = ms(t.elapsed());
    let t = Instant::now();
    let _ = fs.tracked_paths().unwrap_or_default();
    let tracked_warm_ms = ms(t.elapsed());

    // Stage: git precheck, which is pure filesystem work on this thread.
    let ck = feather_core::gitckpt::GitCheckpoints::open(&cwd);
    let files: Vec<String> = writes.clone();
    let t = Instant::now();
    let prepared = ck.precheck(session.dir(), &files);
    let precheck_ms = ms(t.elapsed());

    // Stage: the whole turn, for the ratio.
    let mut whole = Vec::new();
    for i in 0..5 {
        session.push(Message::user_text(format!("turn {i}")))?;
        // Split: the audit flush is a durable commit; everything after it is the
        // snapshot, the file version, and the catalog.
        let t = Instant::now();
        session.flush_audit()?;
        let flush_ms = ms(t.elapsed());
        let t = Instant::now();
        session.commit_turn()?;
        whole.push((flush_ms, ms(t.elapsed())));
    }
    whole.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
    let median = whole[whole.len() / 2];

    println!(
        "history: {} messages\n\
         file_ops          {file_ops_ms:>7.2} ms  ({} reads, {} writes)\n\
         tracked_paths     {tracked_cold_ms:>7.2} ms cold, {tracked_warm_ms:>5.2} ms warm  ({} tracked)\n\
         git precheck      {precheck_ms:>7.2} ms  ({} accepted)\n\
         flush_audit       {:>7.2} ms  (durable commit)\n\
         commit_turn tail  {:>7.2} ms  (snapshot + file version + catalog)",
        messages.len(),
        reads.len(),
        writes.len(),
        owned.len(),
        prepared.as_ref().map(|(a, _)| a.len()).unwrap_or(0),
        median.0,
        median.1,
    );
    let _ = std::fs::remove_dir_all(&root);
    let _: &Path = &cwd;
    Ok(())
}
