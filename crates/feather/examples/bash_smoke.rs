//! Synthetic shell/encoding probe, safe with the user's histories.
use feather_core::tool::AgentTool;
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let tool = feather::bash::BashTool {
        cwd: std::env::current_dir()?,
        timeout: std::time::Duration::from_secs(15),
    };
    let (_tx, rx) = tokio::sync::watch::channel(false);
    let command="printf '中文工具输出\\n'; for word in alpha beta; do printf '%s\\n' \"$word\"; done; pwd; command -v head ls cargo; ls | head -3";
    let out = tool
        .execute(serde_json::json!({"command":command}), rx)
        .await;
    anyhow::ensure!(!out.is_error, "{}", out.text);
    anyhow::ensure!(
        out.text.contains("中文工具输出") && !out.text.contains('\u{fffd}'),
        "Unicode output corrupted"
    );
    println!("{}", out.text);
    Ok(())
}
