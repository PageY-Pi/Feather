//! Dump exactly what the model is billed for on every request.
//!
//! The prompt, the tool definitions and the skills metadata are assembled from
//! several places (`wiring::system_prompt`, `resources::format_skills_for_system_prompt`,
//! `config::project_context`, then `ToolRegistry::defs`). Reviewing any one of them
//! in isolation misses the interaction — a rule stated in the prompt and again in a
//! tool description, or a tool whose parameters contradict the instruction that
//! names it. This writes the assembled text verbatim so a reviewer sees the same
//! bytes the provider does.
//!
//! Written to a file rather than stdout because the point is to hand the whole
//! thing to a model, not to read a diff.
//!
//! Run: `cargo run --release -p feather --example dump_prompt -- /tmp/prompt.md`

use std::sync::Arc;

fn main() -> anyhow::Result<()> {
    let out_path = std::env::args().nth(1).unwrap_or_else(|| {
        std::env::temp_dir()
            .join("feather-prompt-dump.md")
            .display()
            .to_string()
    });
    let root = std::env::current_dir()?;
    let home = feather::config::feather_home();
    let resources = feather::config::resources_for(&root, &home);
    let tmp = std::env::temp_dir().join(feather_core::provider::gen_id("prompt-dump"));
    std::fs::create_dir_all(&tmp)?;
    let settings = feather::config::Settings::default();
    let tasks = Arc::new(feather::tasks::TaskStore::open(&tmp));
    let provider = Arc::new(feather_core::agent::ProviderRef::Faux(
        feather::wiring::faux_provider(),
    ));
    let registry = feather::wiring::build_registry(&root, &tmp, &settings, &tasks, &tmp, &provider);

    // Assemble the prompt the way `build_registry`'s caller does, so the dump is
    // the assembled article and not one of its inputs.
    let mut sys = feather::preferences::apply_system_prompt_override(
        &feather::wiring::system_prompt(),
        &root,
        &home,
    )?;
    sys.push_str(&feather::resources::format_skills_for_system_prompt(
        &resources.skills,
        "read",
    ));
    let ctx = feather::config::project_context(&root, &home);
    if !ctx.is_empty() {
        sys.push_str("\n\nProject-specific instructions and guidelines:\n\n");
        sys.push_str(&ctx);
    }

    let mut out = String::new();
    out.push_str("# Feather: the exact bytes sent on every request\n\n");
    out.push_str(&format!(
        "Base prompt: {} chars (~{} tokens)\n\n",
        sys.chars().count(),
        feather_core::tokenizer::estimate_tokens(&sys)
    ));
    out.push_str("## System prompt (verbatim)\n\n```\n");
    out.push_str(&sys);
    out.push_str("\n```\n\n## Tool definitions (verbatim, as the provider sees them)\n");
    for def in registry.defs() {
        let json = serde_json::to_string_pretty(&def.parameters)?;
        out.push_str(&format!(
            "\n### `{}` (~{} tokens: description {} + parameters {})\n\nDescription:\n\n{}\n\nParameters:\n\n```json\n{}\n```\n",
            def.name,
            feather_core::tokenizer::estimate_tokens(&def.description)
                + feather_core::tokenizer::estimate_tokens(&json),
            feather_core::tokenizer::estimate_tokens(&def.description),
            feather_core::tokenizer::estimate_tokens(&json),
            def.description,
            json
        ));
    }
    out.push_str("\n## Notes on the runtime the prompt describes\n\n");
    out.push_str(
        "- `read` returns `anchor│content` rows; anchors are 5 lowercase letters.\n\
         - Edits are by anchor only; a mismatched line is rejected.\n\
         - Compaction folds old turns into a summary; the raw audit is untouched and \
         `search_context` reads it.\n\
         - `/rewind` is a user command (GUI command palette), not a tool the model can call.\n\
         - Skills are listed by description only; bodies load on demand via `read`.\n",
    );

    std::fs::write(&out_path, &out)?;
    println!("wrote {out_path} ({} chars)", out.chars().count());
    drop(registry);
    drop(tasks);
    let _ = std::fs::remove_dir_all(&tmp);
    Ok(())
}
