//! Which search backend does a given model actually reach, and does it work?
//!
//! Run: `cargo run --release -p feather --example search_backend -- <model> <query> [proxy-url]`
//!
//! The route depends on the network, not on the model: a usable proxy means DuckDuckGo,
//! no proxy means the DeepSeek Search API. Both halves are easy to guess wrong, and the
//! failure mode is invisible — `web_search` reports "DuckDuckGo request failed" without
//! naming the route it picked.
//!
//! Pass a proxy URL as the third argument to test the other half without changing system
//! settings. It is installed with `transport::set_proxy_url`, exactly as startup does, so
//! `web_search` sees it through the same path production code reads.
use feather_core::tool::AgentTool;
use serde_json::json;
use std::time::Instant;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let mut args = std::env::args().skip(1);
    let model = args
        .next()
        .unwrap_or_else(|| "deepseek/deepseek-flash".to_string());
    let query = args.next().unwrap_or_else(|| "rust async".to_string());
    let proxy = args.next();

    println!("model: {model}");
    println!("query: {query}");
    match &proxy {
        Some(url) => {
            feather_core::provider::transport::set_proxy_url(url).expect("valid proxy url");
            println!("route: proxy {url} (injected for this run)");
        }
        None => println!(
            "route: {}",
            feather_core::provider::transport::current_route()
        ),
    }

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
