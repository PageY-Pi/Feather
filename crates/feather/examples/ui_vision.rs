//! Explicit image-only design review. Sends synthetic preview PNGs, not user
//! conversations/configuration. No tools, filesystem access or live session.
use feather_core::{tool::ToolRegistry, Agent, AgentConfig, ContentBlock, Message, Session};
use std::sync::Arc;
fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for c in bytes.chunks(3) {
        let a = c[0] as u32;
        let b = c.get(1).copied().unwrap_or(0) as u32;
        let d = c.get(2).copied().unwrap_or(0) as u32;
        let n = (a << 16) | (b << 8) | d;
        out.push(TABLE[((n >> 18) & 63) as usize] as char);
        out.push(TABLE[((n >> 12) & 63) as usize] as char);
        out.push(if c.len() > 1 {
            TABLE[((n >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if c.len() > 2 {
            TABLE[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}
#[tokio::main(worker_threads = 2)]
async fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let model = args
        .next()
        .ok_or_else(|| anyhow::anyhow!("usage: ui_vision <provider/model> <PNG>..."))?;
    let cat = feather::models::load().ok_or_else(|| anyhow::anyhow!("catalog unavailable"))?;
    let (pid, p, m) = feather::models::find(&cat, &model)
        .ok_or_else(|| anyhow::anyhow!("unknown reviewer model"))?;
    if !m.input.iter().any(|v| v == "image") {
        anyhow::bail!("model not marked image-capable");
    }
    let provider = feather::wiring::provider_from_entry_for(pid, p, m);
    let config=AgentConfig{model:m.id.clone(),api:provider.api(),system_prompt:"You are a critical visual design reviewer. Review only the attached rendered UI screenshots; do not invent things you cannot see. Give compact, actionable visual findings. Focus on top-right control alignment, balance, hit targets, icon consistency, spacing, hierarchy, responsive clipping, readable contrast, and modal composition. Score 1-10 and end with PASS if polished enough to ship, otherwise REVISE. Identify each screenshot by its label. No tools.".into(),max_tokens:2200,temperature:None,context_window:m.context_window.unwrap_or(100_000),thinking_level:None,compaction:Default::default(),compaction_model:None,tools:Vec::new().into()};
    let mut prompt=Message::user_text("Inspect these synthetic Feather screenshots visually. Prioritize concrete visible defects, especially the top right. We are iterating on appearance: request only necessary corrections, not unrelated new features. Conversation and workspace tree are scrollable; a partially visible row at a scroll viewport boundary is expected, not alone evidence of overlap. The API form also scrolls inside its dialog; distinguish scrollable overflow from actual window clipping and inaccessible controls. Judge whether those boundaries and scrollbars are clear. Reply with a verdict for this iteration.");
    for path in args {
        let bytes = std::fs::read(&path)?;
        prompt.content.push(ContentBlock::Text {
            text: format!("Screenshot: {path}"),
        });
        prompt.content.push(ContentBlock::Image {
            media_type: "image/png".into(),
            data: base64(&bytes),
        });
    }
    let mut agent = Agent::new(
        config,
        Session::memory(std::path::Path::new("."), "ui-vision-review"),
        Arc::new(ToolRegistry::new()),
        provider,
        None,
    );
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(160),
        agent.prompt_message_prepared(prompt),
    )
    .await??;
    println!(
        "Reviewer: {model}\nVerdict stop reason: {:?}\n{}",
        result.stop_reason, result.final_text
    );
    Ok(())
}
