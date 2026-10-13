//! Synthetic reproducible microbenchmarks: no network, credentials or user data.
use feather_core::{
    agent::{AgentConfig, ProviderRef},
    compaction,
    provider::{
        faux::{FauxProvider, FauxScript},
        SseParser,
    },
    session::Session,
    tool::ToolRegistry,
    Agent,
};
use std::{hint::black_box, path::Path, sync::Arc, time::Instant};
fn time(iterations: usize, mut work: impl FnMut()) -> std::time::Duration {
    let start = Instant::now();
    for _ in 0..iterations {
        work();
    }
    start.elapsed()
}
// Former line-at-a-time parser, retained ONLY as a benchmark comparison.
fn old_feed(chunk: &[u8]) -> Vec<String> {
    let mut bytes = chunk.to_vec();
    let mut data = String::new();
    let mut out = Vec::new();
    while let Some(end) = bytes.iter().position(|b| *b == b'\n') {
        let line: Vec<u8> = bytes.drain(..=end).collect();
        let line = String::from_utf8_lossy(&line[..end]).to_string();
        let line = line.trim_end_matches('\r');
        if line.is_empty() {
            if !data.is_empty() {
                out.push(std::mem::take(&mut data));
            }
        } else if let Some(rest) = line.strip_prefix("data:") {
            let rest = rest.strip_prefix(' ').unwrap_or(rest);
            if !data.is_empty() {
                data.push('\n');
            }
            data.push_str(rest);
        }
    }
    out
}
fn main() -> anyhow::Result<()> {
    let mut session = Session::memory(Path::new("synthetic"), "perf");
    for n in 0..256 {
        session.push(feather_core::Message::user_text(format!(
            "{n} {}",
            "synthetic text ".repeat(2048)
        )))?;
    }
    let config = AgentConfig {
        model: "synthetic".into(),
        api: feather_core::message::Api::Faux,
        system_prompt: "synthetic".into(),
        max_tokens: 32,
        temperature: None,
        context_window: 1_000_000,
        thinking_level: None,
        compaction: Default::default(),
        compaction_model: None,
        tools: Vec::new().into(),
    };
    let agent = Agent::new(
        config,
        session,
        Arc::new(ToolRegistry::new()),
        ProviderRef::Faux(FauxProvider::new(FauxScript::new())),
        Some(Arc::new(feather_cache::CacheOptimizer::new())),
    );
    let bytes: usize = agent
        .session()
        .messages()
        .iter()
        .map(|m| m.text().len())
        .sum();
    let old = time(200, || {
        black_box(agent.session().messages().to_vec());
    });
    let shared = time(200, || {
        black_box(agent.build_request());
    });
    println!("Request history: {} messages / {} bytes; 200 deep-clones={:.3}ms; 200 shared builds={:.3}ms",agent.session().messages().len(),bytes,old.as_secs_f64()*1000.0,shared.as_secs_f64()*1000.0);
    let rescans = time(200, || {
        black_box(feather_core::tokenizer::estimate_context_tokens(
            agent.session().messages(),
        ));
    });
    let cached = time(200, || {
        black_box(agent.session().context_tokens());
    });
    assert_eq!(
        agent.session().context_tokens(),
        feather_core::tokenizer::estimate_context_tokens(agent.session().messages())
    );
    println!(
        "Context estimate: 200 rescans={:.3}ms; 200 cached reads={:.6}ms",
        rescans.as_secs_f64() * 1000.0,
        cached.as_secs_f64() * 1000.0
    );
    let chunk = (0..10000)
        .map(|n| format!("data: payload-{n}\n\n"))
        .collect::<String>();
    let expected = old_feed(chunk.as_bytes());
    assert_eq!(SseParser::new().feed(chunk.as_bytes()), expected);
    let old = time(20, || {
        black_box(old_feed(chunk.as_bytes()));
    });
    let linear = time(20, || {
        black_box(SseParser::new().feed(chunk.as_bytes()));
    });
    println!(
        "SSE 10000 events: 20 old parses={:.3}ms; 20 linear parses={:.3}ms",
        old.as_secs_f64() * 1000.0,
        linear.as_secs_f64() * 1000.0
    );
    let clean = compaction::without_thinking(agent.session().messages());
    let borrowed = compaction::serialize_messages(agent.session().messages().iter(), 2000);
    assert_eq!(compaction::serialize_conversation(&clean, 2000), borrowed);
    println!("All comparison output equality checks passed; timings are machine-specific microbenchmarks, not end-to-end TPS.");
    Ok(())
}
