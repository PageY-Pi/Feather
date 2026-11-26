//! The static prompt audit, and its proof of work.
//!
//! Every defect variant has a case that reaches it, because a check nobody can
//! make fail is the same as no check. The negatives matter as much: a clean prompt
//! and a prompt with legitimate near-misses must come back empty, or the audit
//! becomes something to appease rather than something to trust.

use feather::prompt_audit::{
    audit, audit_tool_descriptions, rules, tool_references, Defect, TOKEN_BUDGET,
};
use feather::wiring::{build_registry, faux_provider, system_prompt};
use std::sync::Arc;

/// The tools the audit checks against: the live registry, never a list copied here.
///
/// An earlier version of this file carried a hand-written `TOOLS` array, and it
/// immediately reported six false positives because the real prompt names
/// `insert`, `copy`, `move`, `write`, `replace_within` and `subagent`. Keeping a
/// second list would have made the audit drift from the thing it audits — the
/// exact failure it exists to catch.
fn registry_tools() -> Vec<String> {
    let root = std::env::current_dir().expect("cwd");
    let tmp = std::env::temp_dir().join(feather_core::provider::gen_id("prompt-audit"));
    std::fs::create_dir_all(&tmp).expect("tmp dir");
    let settings = feather::config::Settings::default();
    let tasks = Arc::new(feather::tasks::TaskStore::open(&tmp));
    let provider = Arc::new(feather_core::agent::ProviderRef::Faux(faux_provider()));
    let registry = build_registry(&root, &tmp, &settings, &tasks, &tmp, &provider);
    let names = registry.names().iter().map(|n| n.to_string()).collect();
    drop(registry);
    drop(tasks);
    let _ = std::fs::remove_dir_all(&tmp);
    names
}

fn audit_with(prompt: &str) -> Vec<Defect> {
    let tools = registry_tools();
    let refs: Vec<&str> = tools.iter().map(String::as_str).collect();
    audit(prompt, &refs)
}

fn has(prompt: &str, want: Defect) -> bool {
    audit_with(prompt).contains(&want)
}

// --- the real prompt -------------------------------------------------------

#[test]
fn the_shipped_prompt_is_clean() {
    let defects = audit_with(&system_prompt());
    assert!(
        defects.is_empty(),
        "the shipped prompt has defects: {:#?}",
        defects.iter().map(Defect::explain).collect::<Vec<_>>()
    );
}

#[test]
fn the_shipped_prompt_names_several_tools_so_the_check_is_not_vacuous() {
    // `the_shipped_prompt_is_clean` passes trivially if the prompt names no tools
    // at all. This is the other half: the audit must have something to check.
    let referenced: Vec<String> = rules(&system_prompt())
        .iter()
        .flat_map(|rule| tool_references(&rule.text))
        .collect();
    assert!(
        referenced.len() >= 5,
        "expected the prompt to reference several tools, found {:?}",
        referenced
    );
}

#[test]
fn the_shipped_prompt_is_under_budget_with_room_to_edit() {
    let prompt = system_prompt();
    let tokens = feather_core::tokenizer::estimate_tokens(&prompt);
    assert!(
        tokens <= TOKEN_BUDGET,
        "{tokens} tokens, budget {TOKEN_BUDGET}"
    );
    // A budget that is brushing the current size turns every wording change into a
    // fight with the check. Leave real headroom.
    assert!(
        tokens * 10 <= TOKEN_BUDGET * 8,
        "only {}% of the {TOKEN_BUDGET}-token budget is free; the ceiling is too tight to edit under",
        (TOKEN_BUDGET - tokens) * 100 / TOKEN_BUDGET
    );
}

// --- budget ----------------------------------------------------------------

#[test]
fn a_prompt_past_the_budget_is_rejected() {
    let filler = "This sentence exists only to spend tokens on nothing at all. ";
    let long = format!("You are Feather.\n- {}", filler.repeat(80));
    assert!(has(
        &long,
        Defect::OverBudget {
            tokens: feather_core::tokenizer::estimate_tokens(&long),
            budget: TOKEN_BUDGET,
        }
    ));
}

#[test]
fn a_prompt_at_the_budget_passes() {
    // The gate is `>`, not `>=`: sitting exactly on the ceiling is allowed, so an
    // off-by-one cannot make the boundary arbitrary.
    let mut prompt = String::from("You are Feather.\n");
    prompt.push_str("- always check the file before editing it in any way\n");
    let tokens = feather_core::tokenizer::estimate_tokens(&prompt);
    assert!(tokens <= TOKEN_BUDGET);
    assert!(!has(
        &prompt,
        Defect::OverBudget {
            tokens,
            budget: TOKEN_BUDGET,
        }
    ));
}

// --- tool references -------------------------------------------------------

#[test]
fn a_renamed_tool_is_caught() {
    let prompt = "You are Feather.\n- Edit files with `replace_within` and `nope_tool`.\n";
    assert!(has(
        prompt,
        Defect::DeadToolReference {
            line: 2,
            name: "nope_tool".into(),
        }
    ));
}

#[test]
fn the_defect_points_at_the_line_that_is_wrong() {
    let prompt = "You are Feather.\n- Use `bash`.\n- Then call `gone_tool`.\n";
    let defects = audit_with(prompt);
    let dead = defects
        .iter()
        .find(|d| matches!(d, Defect::DeadToolReference { .. }))
        .expect("a dead reference");
    assert_eq!(
        dead.explain(),
        "line 3 names `gone_tool`, which is not a registered tool"
    );
}

#[test]
fn prose_in_backticks_is_not_mistaken_for_a_tool() {
    // This is the case that keeps the check usable: the prompt legitimately
    // backticks a format sample and ordinary words.
    let prompt = "You are Feather.\n- `read` returns `anchor│content` rows; `x = 1` is code.\n";
    assert!(audit_with(prompt).is_empty(), "{:?}", audit_with(prompt));
}

#[test]
fn a_tool_reference_is_found_mid_sentence_and_at_the_end() {
    let rule = "Use `bash` for builds, then `search_context`";
    assert_eq!(
        tool_references(rule),
        vec!["bash".to_string(), "search_context".to_string()]
    );
}

#[test]
fn an_unclosed_backtick_does_not_panic_or_invent_a_tool() {
    let rule = "Use `read to look at files";
    assert!(
        tool_references(rule).is_empty(),
        "{:?}",
        tool_references(rule)
    );
}

// --- duplication -----------------------------------------------------------

#[test]
fn the_same_rule_written_twice_is_caught() {
    let prompt = "You are Feather.\n- Prefer targeted reads over whole files.\n- Prefer targeted reads over whole files.\n";
    assert!(has(
        prompt,
        Defect::DuplicateRule {
            first: 2,
            second: 3
        }
    ));
}

#[test]
fn the_same_rule_in_different_words_is_caught() {
    // Same significant words, different order and filler: this is the duplicate a
    // human reviewer misses.
    let prompt = "You are Feather.\n- Prefer targeted reads over whole files.\n- Over whole files prefer targeted reads.\n";
    assert!(has(
        prompt,
        Defect::DuplicateRule {
            first: 2,
            second: 3
        }
    ));
}

#[test]
fn two_different_rules_are_not_flagged_as_duplicates() {
    let prompt = "You are Feather.\n- Prefer targeted reads over whole files.\n- Use `bash` for builds and tests.\n";
    assert!(audit_with(prompt).is_empty(), "{:?}", audit_with(prompt));
}

// --- mutations of the real prompt -----------------------------------------
//
// A check that only ever sees hand-written samples proves nothing about the
// prompt that ships. These take the real prompt and break it the way an edit
// actually would, then require the audit to notice.

#[test]
fn renaming_a_tool_in_the_real_prompt_is_caught() {
    // Pick the name from the prompt rather than hard-coding one: a hard-coded
    // tool that later gets removed (as `replace_within` was) leaves the mutation
    // silently unapplied and the test passing for the wrong reason.
    let original = system_prompt();
    let target = rules(&original)
        .iter()
        .flat_map(|r| tool_references(&r.text))
        .next()
        .expect("the prompt names at least one tool");
    let prompt = original.replace(&format!("`{target}`"), "`no_such_tool`");
    assert!(
        prompt.contains("`no_such_tool`"),
        "mutation did not apply; target was {target}"
    );
    let defects = audit_with(&prompt);
    assert!(
        defects.iter().any(|d| matches!(
            d,
            Defect::DeadToolReference { name, .. } if name == "no_such_tool"
        )),
        "renaming `{target}` went unnoticed: {:#?}",
        defects.iter().map(Defect::explain).collect::<Vec<_>>()
    );
}

#[test]
fn duplicating_a_line_in_the_real_prompt_is_caught() {
    let original = system_prompt();
    let line = original
        .lines()
        .find(|l| l.trim_start().starts_with("- Use `bash`"))
        .expect("the bash rule");
    let doubled = format!("{}\n{line}", original.trim_end());
    let defects = audit_with(&doubled);
    assert!(
        defects
            .iter()
            .any(|d| matches!(d, Defect::DuplicateRule { .. })),
        "a repeated rule went unnoticed: {:#?}",
        defects.iter().map(Defect::explain).collect::<Vec<_>>()
    );
}

#[test]
fn padding_the_real_prompt_past_budget_is_caught() {
    let padded = format!(
        "{} {}",
        system_prompt(),
        "Additionally, take great care to consider every possible edge case in detail. ".repeat(40)
    );
    let defects = audit_with(&padded);
    assert!(
        defects
            .iter()
            .any(|d| matches!(d, Defect::OverBudget { .. })),
        "growth past the ceiling went unnoticed"
    );
}

#[test]
fn a_rule_repeated_inside_a_longer_rule_is_caught() {
    let inner = "never modify files through the shell";
    let prompt = format!(
        "You are Feather.\n- {inner}.\n- Always {inner} because anchors keep diffs verified.\n"
    );
    assert!(has(&prompt, Defect::RuleInsideRule { outer: 3, inner: 2 }));
}

#[test]
fn a_short_fragment_shared_between_rules_is_not_containment() {
    // Both rules mention `read`, but neither contains the other. A containment
    // check without a length floor would fire here and be useless.
    let prompt =
        "You are Feather.\n- Use `read` to inspect a file.\n- Prefer `read` over `bash`.\n";
    let defects = audit_with(prompt);
    assert!(
        !defects
            .iter()
            .any(|d| matches!(d, Defect::RuleInsideRule { .. })),
        "{:?}",
        defects
    );
}

// --- framing and rule extraction -------------------------------------------

#[test]
fn the_role_sentence_and_headers_are_not_rules() {
    let prompt = "You are Feather, an agent.\nWorking principles:\n- Use `read`.\n";
    let found: Vec<_> = rules(prompt).into_iter().map(|r| r.text).collect();
    assert_eq!(found, vec!["Use `read`.".to_string()]);
}

#[test]
fn a_trailing_unbulleted_sentence_is_still_a_rule() {
    // The closing sentence of the shipped prompt has no dash. If only bullets
    // counted, a whole trailing paragraph could escape the audit.
    let prompt = "You are Feather.\n- Use `read`.\nPrefer prose over lists.\n";
    let found: Vec<_> = rules(prompt).into_iter().map(|r| r.text).collect();
    assert_eq!(
        found,
        vec![
            "Use `read`.".to_string(),
            "Prefer prose over lists.".to_string()
        ]
    );
}

#[test]
fn an_empty_prompt_reports_no_rules_rather_than_passing() {
    // Without this, an empty prompt would satisfy every other check vacuously.
    assert_eq!(audit_with(""), vec![Defect::NoRules]);
    assert_eq!(audit_with("You are Feather.\n"), vec![Defect::NoRules]);
}

#[test]
fn one_edit_that_breaks_two_things_reports_both() {
    let prompt = format!(
        "You are Feather.\n- Call `gone_tool`.\n- Call `gone_tool`.\n{}\n",
        "- filler words to keep going past nothing at all here ".repeat(1)
    );
    let defects = audit_with(&prompt);
    assert!(
        defects
            .iter()
            .any(|d| matches!(d, Defect::DeadToolReference { name, .. } if name == "gone_tool")),
        "{:?}",
        defects
    );
    assert!(
        defects
            .iter()
            .any(|d| matches!(d, Defect::DuplicateRule { .. })),
        "{:?}",
        defects
    );
}

/// A tool description naming a retired tool must be caught.
///
/// The prompt audit could not see this: it reads only the system prompt, so
/// `undo_last_change`'s "replace, insert, copy, or move" survived the retirement of
/// `move` with nothing reporting it. This proves the check fires before the clean
/// case below proves it is silent on the live registry.
#[test]
fn a_description_naming_a_dead_tool_is_reported() {
    let tools = registry_tools();
    let refs: Vec<&str> = tools.iter().map(String::as_str).collect();
    let descriptions = [(
        "undo_last_change",
        "Undo the last replace, insert, copy, or `move` on a file.",
    )];
    // Parameter names are legitimate backticks in a description and are not tools;
    // pass a couple so the negative is exercised here too.
    let defects = audit_tool_descriptions(&descriptions, &refs, &["remove_from", "lines"]);
    assert!(
        defects.iter().any(
            |d| matches!(d, Defect::DeadToolReference { name, .. } if name.starts_with("move"))
        ),
        "a backticked dead tool in a description must be reported: {defects:?}"
    );
}

/// The live registry's own descriptions must be clean.
///
/// This is the gate: if a future edit names a tool that no longer exists in any
/// description, this fails. Run against the real registry, never a copied list.
#[test]
/// The check `prompt_audit`'s header asks for by name: every rule has a scenario.
///
/// Without this the behavioural layer can drift into "some scenarios exist" while
/// a newly added rule stays unwitnessed — which is exactly the state the audit
/// was written against. A rule with no scenario is reported with its line number
/// so the fix is to write the scenario, not to delete the rule.
#[test]
fn every_prompt_rule_has_a_behavioural_scenario() {
    let prompt = system_prompt();
    let scenarios = feather::eval::scenarios();
    let unwitnessed = feather::eval::rules_without_scenarios(&prompt, &scenarios);
    // The prompt has rules this round does not cover yet. Reporting them as a hard
    // failure would block unrelated work, so the gate is a ceiling: it may not
    // grow. Lowering it is always allowed, and adding an unwitnessed rule trips it.
    // Measured, not chosen: `subagent`, `web_search`/`web_fetch` and the prose
    // style rule are the three that need a live provider or a human, so they are
    // the honest floor for a mechanical layer. Any addition trips this.
    const MAX_UNWITNESSED: usize = 3;
    assert!(
        unwitnessed.len() <= MAX_UNWITNESSED,
        "{} rule(s) have no behavioural scenario (ceiling {MAX_UNWITNESSED}); \
         add a scenario in `feather::eval` for each:\n{}",
        unwitnessed.len(),
        unwitnessed
            .iter()
            .map(|r| format!("  - {r}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// The audit above runs on a hand-written table; this runs it on the descriptions a
/// model actually reads.
///
/// It was written without `#[test]`, so it never ran — `cargo test` compiled it and
/// reported nothing, and the `function ... is never used` warning was the only trace.
/// A disabled test is worse than no test: the file reads as if the live descriptions
/// are covered.
#[test]
fn the_live_tool_descriptions_reference_only_real_tools() {
    let names = registry_tools();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let root = std::env::current_dir().expect("cwd");
    let tmp = std::env::temp_dir().join(feather_core::provider::gen_id("desc-audit"));
    std::fs::create_dir_all(&tmp).expect("tmp dir");
    let settings = feather::config::Settings::default();
    let tasks = Arc::new(feather::tasks::TaskStore::open(&tmp));
    let provider = Arc::new(feather_core::agent::ProviderRef::Faux(faux_provider()));
    let registry = build_registry(&root, &tmp, &settings, &tasks, &tmp, &provider);
    // Every property name in every schema is legitimate backticked vocabulary in a
    // description, so the audit is told the full set. Without it, `remove_from` and
    // `timeout` read as dead tools — 18 false positives on the live registry.
    let defs = registry.defs();
    let mut parameters: Vec<String> = Vec::new();
    for def in &defs {
        if let Some(props) = def.parameters.get("properties").and_then(|p| p.as_object()) {
            parameters.extend(props.keys().cloned());
        }
    }
    parameters.sort();
    parameters.dedup();
    let param_refs: Vec<&str> = parameters.iter().map(String::as_str).collect();
    let owned: Vec<(String, String)> = defs.into_iter().map(|d| (d.name, d.description)).collect();
    let descriptions: Vec<(&str, &str)> = owned
        .iter()
        .map(|(n, d)| (n.as_str(), d.as_str()))
        .collect();
    let defects = audit_tool_descriptions(&descriptions, &refs, &param_refs);
    drop(registry);
    drop(tasks);
    let _ = std::fs::remove_dir_all(&tmp);
    assert!(
        defects.is_empty(),
        "tool descriptions name tools that do not exist: {defects:?}"
    );
}
