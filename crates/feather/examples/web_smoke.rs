//! No provider credentials or paid search: public HTTP + DuckDuckGo smoke check.
use feather_core::tool::AgentTool;
#[tokio::main(worker_threads=2)]
async fn main(){
    let(_tx,rx)=tokio::sync::watch::channel(false);
    let fetch=feather::web::WebFetchTool.execute(serde_json::json!({"url":"https://example.com"}),rx.clone()).await;
    println!("fetch error={} · {}",fetch.is_error,fetch.text.chars().take(450).collect::<String>());
    let search=feather::web::WebSearchTool{model:"unconfigured-smoke".into(),base_url:None}.execute(serde_json::json!({"query":"Rust programming official documentation"}),rx).await;
    println!("search error={} · {}",search.is_error,search.text.chars().take(650).collect::<String>());
}
