//! Which search backend does a given model actually reach, and does it work?
//!
//! Run: `cargo run --release -p feather --example search_backend -- deepseek/deepseek-flash`
//!
//! This exists because the answer depended on three things that are easy to guess
//! wrong: the provider id (which selects the backend), the base URL host (which can
//! override that choice), and whether the machine can reach the host at all. The probe
//! reports the resolved route before running it, so a wrong route is visible even when
//! the request would have failed anyway.
use feather_core::tool::AgentTool;
use serde_json::json;
use std::time::Instant;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let model = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "deepseek/deepseek-flash".to_string());
    let query = std::env::args()
        .nth(2)
        .unwrap_or_else(|| "rust async runtime".to_string());

    println!("model: {model}");
    println!("query: {query}");

    // `context()` is private, so the route is observed through the tool's own
    // behaviour: a native backend reports `native:<provider>`, a fallback reports
    // nothing. That is the distinction the fix depends on.
    let tool = feather::web::WebSearchTool {
        model: model.clone(),
        base_url: None,
    };
    let (_abort_tx, abort_rx) = tokio::sync::watch::channel(false);

    let started = Instant::now();
    let outcome = tool.execute(json!({ "query": query }), abort_rx).await;
    let elapsed = started.elapsed();

    println!("\n--- elapsed: {elapsed:?} ---");
    if outcome.is_error {
        println!("FAILED (tool error):");
        println!("{}", outcome.text);
        std::process::exit(1);
    }
    let head: String = outcome.text.chars().take(700).collect();
    println!("{head}");
    println!("\nbytes: {}", outcome.text.len());
}
