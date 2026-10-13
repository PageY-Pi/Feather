//! Metadata-only prompt overhead audit; no skills bodies/user chats/secrets read.
use std::sync::Arc;
fn main() -> anyhow::Result<()> {
    let root = std::env::current_dir()?;
    let home = feather::config::feather_home();
    let resources = feather::config::resources_for(&root, &home);
    let base = feather::wiring::system_prompt();
    let skills = feather::resources::format_skills_for_system_prompt(&resources.skills, "read");
    let tmp = std::env::temp_dir().join(feather_core::provider::gen_id("overhead-audit"));
    std::fs::create_dir_all(&tmp)?;
    let settings = feather::config::Settings::default();
    let tasks = Arc::new(feather::tasks::TaskStore::open(&tmp));
    let provider = Arc::new(feather_core::agent::ProviderRef::Faux(
        feather::wiring::faux_provider(),
    ));
    let registry = feather::wiring::build_registry(&root, &tmp, &settings, &tasks, &tmp, &provider);
    let defs = registry.defs();
    let tools = serde_json::to_string(&defs)?;
    println!(
        "Base instructions: {} chars ≈{} tokens",
        base.chars().count(),
        feather_core::tokenizer::estimate_tokens(&base)
    );
    println!(
        "Tool definitions: {} tools / {} chars ≈{} tokens",
        defs.len(),
        tools.chars().count(),
        feather_core::tokenizer::estimate_tokens(&tools)
    );
    println!(
        "Skills metadata: {} skills / {} chars ≈{} tokens; instructions are NOT loaded",
        resources.skills.len(),
        skills.chars().count(),
        feather_core::tokenizer::estimate_tokens(&skills)
    );
    println!("Static total estimate ≈{} tokens before workspace instructions/conversation (provider tokenizer differs)",feather_core::tokenizer::estimate_tokens(&base)+feather_core::tokenizer::estimate_tokens(&tools)+feather_core::tokenizer::estimate_tokens(&skills));
    drop(registry);
    drop(tasks);
    std::fs::remove_dir_all(tmp)?;
    Ok(())
}
