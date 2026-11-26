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
    // The size numbers above only matter next to the verdict on the prompt: a
    // prompt can be small and still be wrong (a dead tool name, a repeated rule).
    let names: Vec<&str> = registry.names();
    let defects = feather::prompt_audit::audit(&base, &names);
    if defects.is_empty() {
        println!(
            "Prompt audit: clean ({} rules, budget {} tokens, {} free)",
            feather::prompt_audit::rules(&base).len(),
            feather::prompt_audit::TOKEN_BUDGET,
            feather::prompt_audit::TOKEN_BUDGET
                .saturating_sub(feather_core::tokenizer::estimate_tokens(&base))
        );
    } else {
        for defect in &defects {
            println!("Prompt audit: {}", defect.explain());
        }
    }
    drop(registry);
    drop(tasks);
    std::fs::remove_dir_all(tmp)?;
    Ok(())
}
