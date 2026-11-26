//! Does `file_ops` (run on every `commit_turn`) cost more as history grows?
//!
//! `commit_turn` calls `file_ops(&self.messages)` to learn which files the turn
//! touched. It scans the *entire* history extracting tool-call paths, so if the cost
//! tracks history length, an old session pays for its own past on every turn.
//!
//! The bench history has no tool calls, so it cannot show this — measured here with
//! history that actually contains them, at 1x/4x/16x the size.
//!
//! Run: `cargo run --release -p feather --example file_ops_scale`

use feather_core::compaction::file_ops;
use feather_core::message::{Api, ContentBlock, Message, StopReason, ToolCall, Usage};
use std::time::Instant;

/// One round trip: an assistant message that calls a tool, then its result.
fn round(i: usize) -> Vec<Message> {
    let call = ToolCall {
        id: format!("call-{i}"),
        name: if i % 3 == 0 { "read" } else { "replace" }.to_string(),
        arguments: serde_json::json!({ "path": format!("src/file_{}.rs", i % 40) }),
    };
    vec![
        Message::assistant(
            vec![
                ContentBlock::Text {
                    text: format!("working on step {i}"),
                },
                ContentBlock::ToolCall(call),
            ],
            "test-model",
            Api::AnthropicMessages,
            Usage::default(),
            StopReason::ToolUse,
        ),
        Message::tool_result(
            format!("call-{i}"),
            "read",
            format!("file body {i}\n{}", "line\n".repeat(20)),
            false,
        ),
    ]
}

fn history(n: usize) -> Vec<Message> {
    let mut out = Vec::new();
    out.push(Message::user_text("go"));
    for i in 0..n {
        out.extend(round(i));
    }
    out
}

fn main() {
    println!("rounds  messages      file_ops      ns/message");
    for n in [50usize, 200, 800] {
        let msgs = history(n);
        // Median of 5 so a single scheduler hiccup cannot decide the answer.
        let mut samples = Vec::new();
        for _ in 0..5 {
            let t = Instant::now();
            let (reads, writes) = file_ops(&msgs);
            samples.push(t.elapsed().as_secs_f64() * 1e9);
            std::hint::black_box((reads.len(), writes.len()));
        }
        samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let med = samples[samples.len() / 2];
        println!(
            "{n:>6}  {:>8}  {:>10.1} us  {:>10.1}",
            msgs.len(),
            med / 1000.0,
            med / msgs.len() as f64
        );
    }
}
