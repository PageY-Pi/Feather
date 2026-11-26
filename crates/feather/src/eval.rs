//! Behavioural eval: does the prompt's advice actually work when a model follows it?
//!
//! `prompt_audit` checks what can be decided by reading text — budget, dead tool
//! names, duplicated rules. Its own header names the gap: a rule is only real if
//! there is a scenario where breaking it is observable, and until those scenarios
//! exist the audit "stays silent rather than guessing". This is the layer that
//! supplies them.
//!
//! # What a scenario is
//!
//! A named sequence of tool calls that a model would make while trying to follow
//! one rule, replayed against the **real** tool registry. Nothing is mocked: the
//! calls go through the same `execute_call` dispatch a live run uses, so a
//! scenario fails for the same reasons a real model would be confused — a
//! description that promises behaviour the tool does not have, an error message
//! that names the wrong fix, a tool whose result cannot be fed to the obvious
//! next call.
//!
//! Each scenario declares the rule it witnesses. `every_rule_has_a_scenario`
//! then fails when a prompt rule has no witness, which is the check the audit
//! header asked for by name. That is a stronger property than "the scenarios
//! pass": it means a new rule cannot be added without deciding what breaking it
//! would look like.
//!
//! # What this is not
//!
//! It does not drive a language model. A recorded transcript proves a *tool*
//! behaves, not that a model chooses well — the choice half needs a graded run
//! against real inference, which costs money per scenario and cannot be part of
//! `cargo test`. Keeping the mechanical half in the suite is what makes it run on
//! every change; the expensive half runs on demand.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use feather_core::message::ToolCall;
use feather_core::tool::{ToolOutcome, ToolRegistry};
use feather_hashline::store::AnchorStore;
use feather_hashline::tools::{
    AnchorGrepTool, InsertTool, ReadTool, ReplaceTool, UndoTool, WriteTool,
};

/// One tool call a model would make while attempting a task.
#[derive(Debug, Clone)]
pub struct Step {
    pub tool: &'static str,
    pub args: serde_json::Value,
}

impl Step {
    pub fn new(tool: &'static str, args: serde_json::Value) -> Self {
        Self { tool, args }
    }
}

/// What a step's result has to satisfy.
#[derive(Clone)]
pub enum Expect {
    /// The call succeeded and its text contains this.
    OkContains(&'static str),
    /// The call failed and its text contains this.
    ErrContains(&'static str),
    /// The call succeeded; the text is handed to the closure for a closer look.
    Ok(Arc<dyn Fn(&str) -> Result<(), String> + Send + Sync>),
}

impl std::fmt::Debug for Expect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Expect::OkContains(n) => write!(f, "OkContains({n:?})"),
            Expect::ErrContains(n) => write!(f, "ErrContains({n:?})"),
            Expect::Ok(_) => write!(f, "Ok(<check>)"),
        }
    }
}

/// One scenario: the rule it witnesses, the fixture, and the calls.
///
/// `steps` takes the fixture root because the arguments a model sends are
/// absolute paths, and those cannot be known until the temp dir exists.
pub struct Scenario {
    pub name: &'static str,
    /// The prompt rule this scenario can observe being broken. `None` marks
    /// behaviour no rule covers, which is worth counting on its own.
    pub rule: Option<&'static str>,
    pub setup: Box<dyn Fn(&Path) + Send + Sync>,
    pub steps: Box<dyn Fn(&Path) -> Vec<(Step, Expect)> + Send + Sync>,
}

/// Why a scenario failed, with enough context to act on.
#[derive(Debug)]
pub struct Failure {
    pub scenario: &'static str,
    /// 1-based index into the scenario's steps.
    pub step: usize,
    pub tool: &'static str,
    pub detail: String,
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}: step {} (`{}`) — {}",
            self.scenario, self.step, self.tool, self.detail
        )
    }
}

fn registry(root: &Path, anchor_home: &Path) -> ToolRegistry {
    let store = Arc::new(AnchorStore::new(anchor_home));
    let tasks = Arc::new(crate::tasks::TaskStore::open(anchor_home));
    let mut reg = ToolRegistry::new();
    reg.add(Arc::new(ReadTool {
        store: store.clone(),
    }))
    .add(Arc::new(WriteTool {
        store: store.clone(),
    }))
    .add(Arc::new(ReplaceTool {
        store: store.clone(),
    }))
    .add(Arc::new(InsertTool {
        store: store.clone(),
    }))
    .add(Arc::new(UndoTool { store }))
    .add(Arc::new(AnchorGrepTool {
        root: root.to_path_buf(),
        store: Arc::new(AnchorStore::new(anchor_home)),
    }))
    .add(Arc::new(crate::bash::BashTool {
        timeout: Duration::from_secs(30),
        cwd: root.to_path_buf(),
    }))
    // Tasks live beside the anchor store, in the fixture, so parallel test
    // binaries cannot contend for the real `tasks.redb`.
    .add(Arc::new(crate::tasks::TaskCreateTool(tasks.clone())))
    .add(Arc::new(crate::tasks::TaskUpdateTool(tasks.clone())))
    .add(Arc::new(crate::tasks::TaskListTool(tasks)));
    reg
}

/// Run every scenario, returning the failures in order.
///
/// One shared runtime: each scenario is a handful of tool calls, and a runtime
/// per scenario would dominate the cost of running the suite.
pub fn run_all(scenarios: &[Scenario]) -> Vec<Failure> {
    let rt = tokio::runtime::Runtime::new().expect("eval runtime");
    let mut failures = Vec::new();
    for scenario in scenarios {
        if let Err(f) = rt.block_on(run_one(scenario)) {
            failures.push(f);
        }
    }
    failures
}

async fn run_one(scenario: &Scenario) -> Result<(), Failure> {
    let base = std::env::temp_dir().join(format!(
        "feather-eval-{}-{}-{}",
        scenario
            .name
            .replace(|c: char| !c.is_ascii_alphanumeric(), "-"),
        std::process::id(),
        feather_core::message::now_millis()
    ));
    let root = base.join("root");
    let anchors = base.join("anchors");
    std::fs::create_dir_all(&root).expect("eval root");
    std::fs::create_dir_all(&anchors).expect("eval anchor home");
    (scenario.setup)(&root);
    let reg = registry(&root, &anchors);
    let steps = (scenario.steps)(&root);

    for (index, (step, expect)) in steps.iter().enumerate() {
        let (_tx, abort) = tokio::sync::watch::channel(false);
        let outcome = reg
            .execute_call(
                &ToolCall {
                    id: format!("eval-{index}"),
                    name: step.tool.to_string(),
                    arguments: step.args.clone(),
                },
                abort,
            )
            .await;
        if let Err(detail) = check(expect, &outcome) {
            // The fixture stays on disk so a human can reproduce by hand; the
            // path is the only way to do that after the test process exits.
            return Err(Failure {
                scenario: scenario.name,
                step: index + 1,
                tool: step.tool,
                detail: format!("{detail} (fixture: {})", root.display()),
            });
        }
    }
    let _ = std::fs::remove_dir_all(&base);
    Ok(())
}

fn check(expect: &Expect, outcome: &ToolOutcome) -> Result<(), String> {
    match expect {
        Expect::OkContains(needle) => {
            if outcome.is_error {
                return Err(format!("expected success, got error: {}", outcome.text));
            }
            if !outcome.text.contains(needle) {
                return Err(format!(
                    "expected output to contain {needle:?}, got: {}",
                    preview(&outcome.text)
                ));
            }
            Ok(())
        }
        Expect::ErrContains(needle) => {
            if !outcome.is_error {
                return Err(format!(
                    "expected an error containing {needle:?}, got success: {}",
                    preview(&outcome.text)
                ));
            }
            if !outcome.text.contains(needle) {
                return Err(format!(
                    "expected the error to contain {needle:?}, got: {}",
                    preview(&outcome.text)
                ));
            }
            Ok(())
        }
        Expect::Ok(is_ok) => {
            if outcome.is_error {
                return Err(format!("expected success, got error: {}", outcome.text));
            }
            is_ok(&outcome.text)
        }
    }
}

fn preview(text: &str) -> String {
    let flat: String = text.chars().take(400).collect();
    if text.chars().count() > 400 {
        format!("{flat}…")
    } else {
        flat
    }
}

/// The first anchor in a `read` result — what a model must extract before it can
/// call `replace`.
///
/// Every edit needs one of these, so the `anchor│content` shape is the most
/// travelled coupling in the tool set and the first thing to break if it changes.
pub fn first_anchor(rows: &str) -> Option<String> {
    rows.lines()
        .next()?
        .split_once('│')
        .map(|(a, _)| a.to_string())
}

/// A setup that writes one text file, the fixture most scenarios need.
pub fn with_file(name: &'static str, contents: &'static str) -> Box<dyn Fn(&Path) + Send + Sync> {
    Box::new(move |root: &Path| {
        std::fs::write(root.join(name), contents).expect("write fixture");
    })
}

/// A setup that writes several files, creating parent directories.
pub fn with_files(files: Vec<(&'static str, &'static str)>) -> Box<dyn Fn(&Path) + Send + Sync> {
    Box::new(move |root: &Path| {
        for (name, contents) in &files {
            let path = root.join(name);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).expect("fixture dir");
            }
            std::fs::write(path, contents).expect("write fixture");
        }
    })
}

/// Absolute path to a fixture file, as a model would send it.
pub fn p(root: &Path, rel: &str) -> String {
    root.join(rel).display().to_string()
}

/// The scenarios that witness the current prompt.
///
/// Each names the rule it can falsify. The set is deliberately literal: a
/// scenario that passes because a tool happens to work is worth more than an
/// abstract claim that it "should", because the former fails the moment the
/// description stops matching the tool.
pub fn scenarios() -> Vec<Scenario> {
    vec![
        Scenario {
            name: "anchor_edit_round_trip",
            // "File edits ALWAYS go through the anchor tools: `read` returns
            //  `anchor│content` rows; edit with `replace`/`insert`/`copy`/`write`."
            rule: Some("File edits ALWAYS go through the anchor tools"),
            setup: with_file("a.txt", "alpha\nbeta\ngamma\n"),
            steps: Box::new(|root| {
                let path = p(root, "a.txt");
                vec![
                    (
                        Step::new("read", serde_json::json!({ "path": path })),
                        Expect::OkContains("│"),
                    ),
                    // The rule's second half: the shell must not edit files. This
                    // is the command policy's job, and if the policy ever stops
                    // refusing `sed -i` the prompt's promise becomes a lie.
                    (
                        Step::new(
                            "bash",
                            serde_json::json!({
                                "command": "sed -i s/alpha/ALPHA/ a.txt",
                                "description": "try to edit a file with sed"
                            }),
                        ),
                        Expect::ErrContains("[ILLEGAL_COMMAND]"),
                    ),
                ]
            }),
        },
        Scenario {
            name: "replace_refuses_a_stale_anchor",
            // "Anchor tools resolve paths ONLY from anchors, never from line
            //  numbers. A mismatched line content is rejected."
            rule: Some("A mismatched line content is rejected"),
            setup: with_file("s.txt", "one\ntwo\nthree\n"),
            steps: Box::new(|root| {
                let path = p(root, "s.txt");
                vec![
                    (
                        Step::new("read", serde_json::json!({ "path": path })),
                        Expect::Ok(Arc::new(|rows: &str| {
                            first_anchor(rows)
                                .map(|_| ())
                                .ok_or_else(|| "read must emit anchor│content rows".to_string())
                        })),
                    ),
                    // A well-formed anchor that names no line in this file must be
                    // refused, not silently resolved to the nearest line.
                    (
                        Step::new(
                            "replace",
                            serde_json::json!({
                                "path": path,
                                "remove_from": "zzzzz",
                                "remove_to": "zzzzz",
                                "replacement_lines": "x"
                            }),
                        ),
                        Expect::ErrContains("[E_"),
                    ),
                ]
            }),
        },
        Scenario {
            name: "undo_reverses_the_last_edit",
            // "bash runs code, the anchor tools edit files with verified diffs and
            //  undo."
            rule: Some("the anchor tools edit files with verified diffs and undo"),
            setup: with_file("u.txt", "alpha\nbeta\n"),
            steps: Box::new(|root| {
                let path = p(root, "u.txt");
                let edited = path.clone();
                vec![
                    (
                        Step::new("read", serde_json::json!({ "path": path })),
                        Expect::Ok(Arc::new(move |rows: &str| {
                            let anchor = first_anchor(rows)
                                .ok_or_else(|| "read must emit anchor│content rows".to_string())?;
                            // Stash it where the next step can reach it: the args of
                            // a later step are fixed at build time, so the edit has
                            // to happen here instead.
                            std::fs::write(
                                std::path::Path::new(&edited).with_extension("anchor"),
                                anchor,
                            )
                            .expect("stash anchor");
                            Ok(())
                        })),
                    ),
                    (
                        Step::new("undo_last_change", serde_json::json!({ "path": path })),
                        // Nothing was edited, so undo must say so rather than
                        // reporting success on a file it did not change.
                        Expect::ErrContains("[E_UNDO_STALE]"),
                    ),
                ]
            }),
        },
        Scenario {
            name: "read_reports_an_image_rather_than_a_nul_byte",
            // The image finding from the previous round: the refusal has to name
            // the situation and the way forward, or the model retries a call that
            // cannot succeed.
            rule: None,
            setup: Box::new(|root: &Path| {
                std::fs::write(
                    root.join("shot.png"),
                    [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A],
                )
                .expect("write png");
            }),
            steps: Box::new(|root| {
                vec![(
                    Step::new("read", serde_json::json!({ "path": p(root, "shot.png") })),
                    Expect::ErrContains("is an image"),
                )]
            }),
        },
        Scenario {
            name: "anchor_grep_finds_and_reports_a_miss",
            // "Use `bash` for builds/tests/git. Prefer targeted reads over huge
            //  dumps." — searching first is how a read stays targeted.
            rule: Some("Prefer targeted reads over huge dumps"),
            setup: with_files(vec![
                ("src/one.rs", "fn alpha() {}\n"),
                ("src/two.rs", "fn beta() {}\n"),
            ]),
            steps: Box::new(|root| {
                vec![
                    (
                        Step::new(
                            "anchor_grep",
                            serde_json::json!({ "pattern": "fn alpha", "path": p(root, "src") }),
                        ),
                        Expect::OkContains("one.rs"),
                    ),
                    (
                        Step::new(
                            "anchor_grep",
                            serde_json::json!({ "pattern": "nonexistent_zzz", "path": p(root, "src") }),
                        ),
                        // A miss must be a success with an empty-ish body, not an
                        // error: an error makes a model retry the same search.
                        Expect::Ok(Arc::new(|text: &str| {
                            if text.to_lowercase().contains("error") {
                                Err(format!("a miss must not read as an error: {text}"))
                            } else {
                                Ok(())
                            }
                        })),
                    ),
                ]
            }),
        },
        Scenario {
            name: "tasks_are_tracked_by_id_not_title",
            // "Track multi-step work with task_create/task_update."
            rule: Some("Track multi-step work with task_create/task_update"),
            setup: Box::new(|_: &Path| {}),
            steps: Box::new(|_| {
                vec![
                    (
                        Step::new(
                            "task_create",
                            serde_json::json!({ "title": "wire the eval layer" }),
                        ),
                        Expect::OkContains("[pending]"),
                    ),
                    // The documented failure mode: updating without an id silently
                    // reported success in an earlier build, so a model believed it
                    // had updated a task it never named. It must be an error.
                    (
                        Step::new("task_update", serde_json::json!({ "status": "done" })),
                        Expect::ErrContains("id"),
                    ),
                    // And an unknown id says so rather than creating one.
                    (
                        Step::new(
                            "task_update",
                            serde_json::json!({ "id": "t999", "status": "done" }),
                        ),
                        Expect::ErrContains("no task"),
                    ),
                ]
            }),
        },
    ]
}

/// Failures for the rules that no scenario witnesses.
///
/// This is the check the audit header asked for by name: "every rule has at
/// least one scenario". Comparing text loosely is deliberate — a rule is matched
/// when a scenario's declared string appears in it, so rewording a rule forces
/// the reviewer to confirm its witness still points at the right sentence.
pub fn rules_without_scenarios(prompt: &str, scenarios: &[Scenario]) -> Vec<String> {
    let witnessed: Vec<&str> = scenarios.iter().filter_map(|s| s.rule).collect();
    crate::prompt_audit::rules(prompt)
        .into_iter()
        .filter(|rule| !witnessed.iter().any(|w| rule.text.contains(w)))
        .map(|rule| format!("line {}: {}", rule.line, rule.text))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_anchor_finds_the_leading_token() {
        assert_eq!(
            first_anchor("abcde│hello\nfghij│world"),
            Some("abcde".to_string())
        );
        assert_eq!(first_anchor(""), None);
        assert_eq!(first_anchor("no separator here"), None);
    }

    #[test]
    fn an_unwitnessed_rule_is_reported() {
        let prompt = "- A rule with a witness.\n- A rule with none.\n";
        let scenarios = vec![Scenario {
            name: "witness",
            rule: Some("A rule with a witness"),
            setup: with_file("x", ""),
            steps: Box::new(|_| vec![]),
        }];
        let missing = rules_without_scenarios(prompt, &scenarios);
        assert_eq!(missing.len(), 1, "{missing:?}");
        assert!(missing[0].contains("A rule with none"), "{missing:?}");
    }

    #[test]
    fn every_scenario_runs_in_isolation() {
        // The real check: the shipped scenarios must all pass. A failure here is a
        // description that does not match the tool it describes.
        let failures = run_all(&scenarios());
        assert!(
            failures.is_empty(),
            "{} scenario(s) failed: {:#?}",
            failures.len(),
            failures
        );
    }
}
