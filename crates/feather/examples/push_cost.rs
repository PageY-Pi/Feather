//! Does `Session::push` re-materialise the whole message text on every call?
//!
//! `Message::text()` concatenates every text block into a fresh `String`. `push`
//! called it twice per message -- once per prefix test -- so a tool result carrying
//! a few KB of output was copied twice before being buffered. The bench cannot see
//! this because its messages are a short sentence; a real tool result is not.
//!
//! This measures `push` at three message sizes, so the claim is checked at the size
//! the code actually runs at rather than at the size the bench happens to use.
//!
//! Run: `cargo run --release -p feather --example push_cost`

use feather_core::message::Message;
use feather_core::session::Session;
use std::time::Instant;

/// Median of `n` timed `push` calls, in microseconds.
fn timed_push(session: &mut Session, body: &str, n: usize) -> f64 {
    let mut times = Vec::with_capacity(n);
    for i in 0..n {
        let msg = Message::tool_result(format!("call-{i}"), "bash", body.to_string(), false);
        let t = Instant::now();
        session.push(msg).expect("push");
        times.push(t.elapsed().as_secs_f64() * 1e6);
    }
    times.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    times[times.len() / 2]
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = std::env::temp_dir().join(format!("feather-push-{}", std::process::id()));
    let home = root.join("home");
    let cwd = root.join("cwd");
    std::fs::create_dir_all(&home)?;
    std::fs::create_dir_all(&cwd)?;

    println!(
        "{:>10}  {:>12}  {:>12}",
        "body bytes", "push (us)", "us per KiB"
    );
    for kib in [1usize, 8, 64, 256] {
        let mut session = Session::new(&home.join(format!("s{kib}")), &cwd)?;
        let body = "x".repeat(kib * 1024);
        // Warm: the first push allocates the buffers the rest reuse.
        let _ = timed_push(&mut session, &body, 1);
        let us = timed_push(&mut session, &body, 15);
        println!(
            "{:>10}  {:>12.1}  {:>12.1}",
            kib * 1024,
            us,
            us / kib as f64
        );
        drop(session);
    }
    let _ = std::fs::remove_dir_all(&root);
    Ok(())
}
