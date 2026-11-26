//! Native child agents: independent contexts, explicit models/tools and bounded receipts.
use crate::bash::BashTool;
use feather_core::{
    agent::{Agent, AgentConfig, AgentEvent, ProviderRef},
    provider::ThinkingLevel,
    substore::SubagentStore,
    tool::{AgentTool, BoxFut, ToolOutcome, ToolRegistry},
};
use feather_hashline::{store::AnchorStore, tools::*};
use std::{path::PathBuf, sync::Arc};

pub struct SubagentTool {
    pub provider: Arc<ProviderRef>,
    pub root: PathBuf,
    pub session_dir: PathBuf,
    pub model: String,
    pub max_tokens: u32,
    pub context_window: u64,
    pub bash_timeout: std::time::Duration,
    pub inherited_tools: Option<Arc<ToolRegistry>>,
    pub use_policy: bool,
}
/// Parent receipts are capped, but the full final answer stays in child redb.
pub fn bounded(text: &str, bytes: usize, lines: usize) -> String {
    let mut end = 0;
    let mut count = 1;
    for (i, c) in text.char_indices() {
        if i + c.len_utf8() > bytes || count > lines {
            break;
        }
        end = i + c.len_utf8();
        if c == '\n' {
            count += 1;
        }
    }
    if end < text.len() {
        format!(
            "{}\n[Truncated; full result is in subagents.redb.]",
            &text[..end]
        )
    } else {
        text.into()
    }
}

const POLICY_DOC: &str = "subagent-policy";
/// The subagent allowlist lives in its config.redb document. Legacy local JSON
/// and Pi's allowlist are imported once; runtime never depends on them again.
/// Reduce a tool name to what the registry keys on.
///
/// Real calls sent `functions.read` and `functions.bash`, which is the OpenAI
/// tool-call namespace leaking into the argument. The registry has no such
/// prefix, so the whole subagent call was refused over decoration. Also handles
/// `read()` and `read:` for the same reason, and trims whitespace.
fn normalise_tool_name(raw: &str) -> &str {
    let n = raw.trim();
    let n = n.strip_prefix("functions.").unwrap_or(n);
    let n = n.strip_suffix("()").unwrap_or(n);
    n.trim_end_matches(':')
}

/// Explain an unresolvable model id in terms the caller can act on.
///
/// A bare list of every model would be hundreds of lines and would not say which
/// one was intended. Real failures were close misses — `gpt-5.2`, `sonnet`,
/// `gpt-5` — so rank by token overlap with what was asked, and always mention the
/// tool that enumerates the authoritative list.
/// Length of the shared prefix of two strings, in chars.
fn common_prefix_len(a: &str, b: &str) -> usize {
    a.chars().zip(b.chars()).take_while(|(x, y)| x == y).count()
}

/// Number of distinct character trigrams the two strings share.
///
/// Chosen over edit distance because model ids differ by leading provider text
/// as often as by version, and trigram overlap ranks a shared distinctive stem
/// (`gpt-5.2`) above a shared common word (`model`).
fn trigram_overlap(a: &str, b: &str) -> usize {
    fn grams(s: &str) -> std::collections::HashSet<[char; 3]> {
        let chars: Vec<char> = s.chars().collect();
        chars.windows(3).map(|w| [w[0], w[1], w[2]]).collect()
    }
    let x = grams(a);
    let y = grams(b);
    x.intersection(&y).count()
}

fn suggest_models(cat: &crate::models::ModelsFile, asked: &str) -> String {
    // Compare on the part after any provider prefix and lowered, because the
    // misses were mostly about casing and vendor spelling rather than intent.
    let needle = asked.rsplit('/').next().unwrap_or(asked).to_lowercase();
    let needle_bits: std::collections::HashSet<&str> = needle
        .split(|c: char| !c.is_alphanumeric())
        .filter(|s| !s.is_empty())
        .collect();

    let mut scored: Vec<(usize, String)> = Vec::new();
    for (pid, p) in &cat.providers {
        if !crate::auth::credential_configured(pid, p.api_key.as_deref()) {
            continue;
        }
        for m in &p.models {
            let id = format!("{pid}/{}", m.id);
            let lower = m.id.to_lowercase();
            let bits: std::collections::HashSet<&str> = lower
                .split(|c: char| !c.is_alphanumeric())
                .filter(|s| !s.is_empty())
                .collect();
            let overlap = needle_bits.intersection(&bits).count();
            // Substring hits are stronger than token overlap, so a near miss like
            // `gpt-5.2` surfaces the versioned ids that actually exist.
            // Score on character n-grams rather than whole tokens. A near miss
            // like `gpt-5.2` shares no token with `gpt-5.2-codex` — the dot splits
            // into different tokens on each side — yet shares almost every
            // trigram, so token overlap alone reported "no resemblance".
            let common = common_prefix_len(&lower, &needle);
            let grams = trigram_overlap(&lower, &needle);
            let score = overlap * 2
                + usize::from(lower.contains(&needle) || needle.contains(&lower))
                + common
                + grams;
            scored.push((score, id));
        }
    }
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    let close: Vec<&str> = scored
        .iter()
        .filter(|(score, _)| *score > 0)
        .take(8)
        .map(|(_, id)| id.as_str())
        .collect();

    let mut out = String::new();
    if close.is_empty() {
        // Nothing resembled the request, so a ranked hint has nothing to rank.
        // Guessing already failed once; leaving the caller no concrete id sends it
        // straight back to guessing. Show a few real ones instead of a dead end.
        out.push_str("No configured model resembles that name. Usable ids include:\n");
        let mut sample: Vec<String> = scored.iter().map(|(_, id)| id.clone()).collect();
        sample.sort();
        if sample.is_empty() {
            out.push_str("  (none configured)\n");
        }
        for id in sample.iter().take(5) {
            out.push_str("  ");
            out.push_str(id);
            out.push('\n');
        }
    } else {
        out.push_str("Closest configured models:\n");
        for id in close {
            out.push_str("  ");
            out.push_str(id);
            out.push('\n');
        }
    }
    out.push_str(
        "Call `subagent_models` for the full list of usable ids, or omit `model` to inherit the parent's.",
    );
    out
}

fn policy() -> Result<Option<serde_json::Value>, String> {
    let home = crate::config::feather_home();
    crate::config_store::read(&home, POLICY_DOC)
        .map(|value| value.filter(|v| !v.as_object().is_some_and(|m| m.is_empty())))
        .map_err(|_| "subagent policy redb read failed".into())
}
impl SubagentTool {
    async fn run(
        &self,
        args: serde_json::Value,
        mut abort: tokio::sync::watch::Receiver<bool>,
        progress: Option<tokio::sync::mpsc::UnboundedSender<String>>,
    ) -> ToolOutcome {
        if args.get("resume").is_some() {
            return ToolOutcome::err("subagent: child resume is not supported yet");
        }
        let prompt = match args
            .get("task")
            .or_else(|| args.get("prompt"))
            .and_then(|v| v.as_str())
        {
            Some(t) if !t.trim().is_empty() => t.to_string(),
            _ => return ToolOutcome::err("subagent: task is required"),
        };
        let label = args
            .get("label")
            .or_else(|| args.get("description"))
            .and_then(|v| v.as_str())
            .unwrap_or("subtask");
        let root = match args.get("cwd").and_then(|v| v.as_str()) {
            Some(p) => {
                let p = PathBuf::from(p);
                if p.is_absolute() {
                    p
                } else {
                    self.root.join(p)
                }
            }
            None => self.root.clone(),
        };
        if !root.is_dir() {
            return ToolOutcome::err("subagent: cwd is not a directory");
        }
        let policy = if self.use_policy {
            match policy() {
                Ok(p) => p,
                Err(e) => return ToolOutcome::err(e),
            }
        } else {
            None
        };
        let default_model = policy
            .as_ref()
            .filter(|_| self.inherited_tools.is_some())
            .and_then(|p| p["default"].as_str());
        let requested = args.get("model").and_then(|v| v.as_str()).or(default_model);
        if let (Some(id), Some(policy)) = (requested, policy.as_ref()) {
            if policy["enabled"].as_bool().unwrap_or(true)
                && !policy["allowed"]
                    .as_array()
                    .is_some_and(|allowed| allowed.iter().any(|m| m["id"].as_str() == Some(id)))
            {
                return ToolOutcome::err(format!("subagent: model is not allowed: {id}"));
            }
        }
        let mut provider = (*self.provider).clone();
        let mut model = self
            .model
            .split_once('/')
            .map(|(_, m)| m)
            .unwrap_or(&self.model)
            .to_string();
        let mut context = self.context_window;
        let mut thinking = None;
        if let Some(id) = requested {
            let Some(cat) = crate::models::load() else {
                return ToolOutcome::err("subagent: model catalog unavailable");
            };
            let Some((pid, p, m)) = crate::models::find(&cat, id) else {
                // Name what did not match and what exists. The bare "unknown
                // model" sent the model guessing again — six real calls invented
                // `gpt-5.2`, `sonnet`, and `gpt-4o`, none of which exist here.
                return ToolOutcome::err(format!(
                    "subagent: unknown model `{id}`.\n{}",
                    suggest_models(&cat, id)
                ));
            };
            if !crate::auth::credential_configured(pid, p.api_key.as_deref()) {
                // A model can exist in the catalog and still be unusable. Say
                // which, and point at the list that filters by credential, so the
                // next attempt is not another guess.
                return ToolOutcome::err(format!(
                    "subagent: `{id}` exists but has no credential configured for provider `{pid}`, \
                     so it cannot be called.\nUse one that is ready: call `subagent_models`, or omit \
                     `model` to inherit the parent's."
                ));
            }
            provider = crate::wiring::provider_from_entry_for(pid, p, m);
            model = m.id.clone();
            // Same global-vs-model rule as the parent agent.
            context = crate::models::effective_context_window(self.context_window, Some(m));
            if let Some(want) = args
                .get("thinking")
                .and_then(|v| v.as_str())
                .or_else(|| policy.as_ref().and_then(|p| p["defaultThinking"].as_str()))
            {
                if !crate::models::thinking_levels(m, p)
                    .iter()
                    .any(|l| l == want)
                {
                    return ToolOutcome::err(format!(
                        "subagent: unsupported thinking level {want}"
                    ));
                }
                thinking = ThinkingLevel::parse(want);
            }
        } else if let Some(want) = args.get("thinking").and_then(|v| v.as_str()) {
            thinking = ThinkingLevel::parse(want);
            if thinking.is_none() {
                return ToolOutcome::err("subagent: invalid thinking level");
            }
        }
        let id = feather_core::provider::gen_id("sub");
        let store = match SubagentStore::open(&self.session_dir) {
            Ok(s) => Arc::new(s),
            Err(e) => return ToolOutcome::err(format!("subagent: store failed: {e}")),
        };
        let anchor_home = self.session_dir.join("children").join(&id);
        let anchors =
            Arc::new(AnchorStore::new(&anchor_home).with_snapshots(&self.session_dir, &self.root));
        let mut all = ToolRegistry::new();
        all.add(Arc::new(crate::web::WebFetchTool))
            .add(Arc::new(crate::web::WebSearchTool {
                model: requested.unwrap_or(&self.model).to_string(),
                base_url: None,
            }));
        all.add(Arc::new(ReadTool {
            store: anchors.clone(),
        }))
        .add(Arc::new(WriteTool {
            store: anchors.clone(),
        }))
        .add(Arc::new(ReplaceTool {
            store: anchors.clone(),
        }))
        .add(Arc::new(InsertTool {
            store: anchors.clone(),
        }))
        .add(Arc::new(CopyTool {
            store: anchors.clone(),
        }))
        .add(Arc::new(MoveTool {
            store: anchors.clone(),
        }))
        .add(Arc::new(UndoTool {
            store: anchors.clone(),
        }))
        .add(Arc::new(AnchorGrepTool {
            root: root.clone(),
            store: anchors,
        }))
        .add(Arc::new(BashTool {
            cwd: root.clone(),
            timeout: self.bash_timeout,
        }));
        // Inherited optional tools retain their implementations; anchors remain
        // isolated to this child, never sharing editable handles with siblings.
        if let Some(parent) = &self.inherited_tools {
            for name in parent.names() {
                if all.get(name).is_none()
                    && !matches!(name, "subagent" | "subagent_models" | "agent")
                {
                    all.add(parent.get(name).unwrap());
                }
            }
        }
        let mut registry = ToolRegistry::new();
        let names: Vec<String> = match args.get("tools") {
            Some(serde_json::Value::Array(names)) => {
                let mut out = Vec::new();
                for name in names {
                    let Some(n) = name.as_str() else {
                        return ToolOutcome::err("subagent: tools must be string names");
                    };
                    // Models arrive with provider-decorated names — `functions.read`
                    // from the OpenAI tool-call format, or `read()` from prose — and
                    // rejecting the whole call over the decoration loses the work.
                    // Strip the wrapper; the bare name is what the registry keys on.
                    out.push(normalise_tool_name(n).into());
                }
                out
            }
            Some(_) => return ToolOutcome::err("subagent: tools must be an array"),
            None => self
                .inherited_tools
                .as_ref()
                .map(|tools| tools.names())
                .unwrap_or_else(|| all.names())
                .into_iter()
                .filter(|name| !matches!(*name, "subagent" | "subagent_models" | "agent"))
                .map(String::from)
                .collect(),
        };
        for name in names {
            match all.get(&name) {
                Some(t) => {
                    registry.add(t);
                }
                None => return ToolOutcome::err(format!("subagent: unsupported tool {name}")),
            }
        }
        if let Err(e) = store.start(&id, label, &prompt, &model) {
            return ToolOutcome::err(e.to_string());
        }
        let config = AgentConfig {
            model: model.clone(),
            api: provider.api(),
            system_prompt: crate::wiring::system_prompt(),
            max_tokens: self.max_tokens,
            temperature: None,
            context_window: context,
            thinking_level: thinking,
            compaction: Default::default(),
            compaction_model: None,
            tools: registry.defs().into(),
        };
        let mut agent = Agent::new(
            config,
            feather_core::session::Session::child(&root, id.clone(), store.clone()),
            Arc::new(registry),
            provider,
            Some(Arc::new(feather_cache::CacheOptimizer::new())),
        );
        agent.reset_abort();
        if *abort.borrow() {
            agent.abort();
        }
        let abort_tx = agent.abort_sender();
        let mut events = agent.subscribe();
        let timeout = args
            .get("timeoutMs")
            .and_then(|v| v.as_u64())
            .unwrap_or(300000);
        let run = agent.prompt_message_prepared(feather_core::Message::user_text(prompt));
        let mut run = Box::pin(run);
        let deadline = tokio::time::sleep(std::time::Duration::from_millis(timeout));
        tokio::pin!(deadline);
        let mut partial = String::new();
        let mut last = std::time::Instant::now() - std::time::Duration::from_secs(1);
        let mut parent_open = true;
        let mut heartbeat = tokio::time::interval(std::time::Duration::from_millis(100));
        // One header line for every progress tick, so the card can show which
        // child is running on which model. The model is resolved above from
        // the request or the parent's default; without it the reader cannot
        // tell two concurrent children apart.
        let header = format!("{label} · {model}");
        if let Some(p) = &progress {
            let _ = p.send(format!("{header}\nStarting child"));
        }
        let mut timed_out = false;
        let mut persist_error = None;
        let result = loop {
            tokio::select! {
             result=&mut run=>break result,
             _=&mut deadline,if !timed_out=>{timed_out=true;let _=abort_tx.send(true);},
             changed=abort.changed(),if parent_open=>{if changed.is_err(){parent_open=false;}else if *abort.borrow(){let _=abort_tx.send(true);}},
             _=heartbeat.tick()=>{if let Some(p)=&progress{let _=p.send(format!("{header}\n{}",bounded(&partial,2048,40)));}},
             Ok(event)=events.recv()=>{match event{
              AgentEvent::AssistantMessage(message)=>{let _=message;partial.clear();},
              AgentEvent::TextDelta(text)=>{if partial.len()<2048{partial.push_str(&bounded(&text,2048-partial.len(),40));}},
              AgentEvent::ToolStart{call}=>{partial=format!("Running {}",call.name);},_=>{}
             }if last.elapsed()>=std::time::Duration::from_millis(100){if let Some(p)=&progress{let _=p.send(format!("{header}\n{}",bounded(&partial,2048,40)));}last=std::time::Instant::now();}}
            }
        };
        drop(run);
        // Session::child writes an append-only audit as messages enter context;
        // do not overwrite it with the compacted final context.
        let (text, error) = match result {
            Ok(r) => {
                let failed = timed_out
                    || matches!(
                        r.stop_reason,
                        feather_core::StopReason::Aborted | feather_core::StopReason::Error
                    );
                let text = if timed_out {
                    "subagent: timed out".into()
                } else if r.stop_reason == feather_core::StopReason::Aborted {
                    "subagent: aborted".into()
                } else if r.stop_reason == feather_core::StopReason::Error {
                    format!("subagent: provider stream failed\n{}", r.final_text)
                } else {
                    r.final_text
                };
                (text, failed)
            }
            Err(e) => (
                if timed_out {
                    "subagent: timed out".into()
                } else {
                    e.to_string()
                },
                true,
            ),
        };
        let durable = if error {
            store.fail(&id, &text)
        } else {
            store.finish(&id, &text)
        };
        if let Err(e) = durable {
            persist_error = Some(e.to_string());
        }
        if let Some(e) = persist_error {
            return ToolOutcome::err(format!("subagent: durable record failed: {e}"));
        }
        let receipt = format!(
            "[status={} session={}#{}]\n{}",
            if error { "error" } else { "done" },
            self.session_dir.join("subagents.redb").display(),
            id,
            bounded(&text, 8192, 200)
        );
        if let Some(p) = progress {
            let _ = p.send(receipt.clone());
        }
        ToolOutcome {
            text: receipt,
            is_error: error,
            details: Some(serde_json::json!({"id":id,"model":model})),
        }
    }
}
impl AgentTool for SubagentTool {
    fn name(&self) -> &'static str {
        "subagent"
    }
    fn description(&self) -> &'static str {
        "Delegate an isolated task to a child agent. Only `task` is required; omit `model` to inherit the parent's — a guessed name will not resolve. Real work needs minutes, so keep `timeoutMs` at 300000 or more. Concurrent calls run in parallel; send independent tasks in one message. Full child records are in redb."
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({"type":"object","properties":{"task":{"type":"string","description":"Self-contained; the child cannot see this conversation"},"label":{"type":"string"},"model":{"type":"string","description":"Exact id from subagent_models. Omit to inherit the parent's."},"thinking":{"type":"string","enum":["off","minimal","low","medium","high","xhigh","max"]},"tools":{"type":"array","items":{"type":"string"},"description":"Bare names such as read, bash. Omit to inherit all."},"cwd":{"type":"string"},"timeoutMs":{"type":"integer","minimum":1,"description":"Default 300000. Real work needs minutes."}},"required":["task"]})
    }
    fn parallel_safe(&self) -> bool {
        true
    }
    fn execute<'a>(
        &'a self,
        args: serde_json::Value,
        abort: tokio::sync::watch::Receiver<bool>,
    ) -> BoxFut<'a> {
        Box::pin(self.run(args, abort, None))
    }
    fn execute_with_progress<'a>(
        &'a self,
        args: serde_json::Value,
        abort: tokio::sync::watch::Receiver<bool>,
        progress: tokio::sync::mpsc::UnboundedSender<String>,
    ) -> BoxFut<'a> {
        Box::pin(self.run(args, abort, Some(progress)))
    }
}
pub struct SubagentModelsTool;
impl AgentTool for SubagentModelsTool {
    fn name(&self) -> &'static str {
        "subagent_models"
    }
    fn description(&self) -> &'static str {
        "List configured, authenticated child models and supported thinking levels. No child starts."
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({"type":"object","properties":{}})
    }
    fn execute<'a>(
        &'a self,
        _args: serde_json::Value,
        _abort: tokio::sync::watch::Receiver<bool>,
    ) -> BoxFut<'a> {
        Box::pin(async move {
            let Some(cat) = crate::models::load() else {
                return ToolOutcome::err("model catalog unavailable");
            };
            let allowed = match policy() {
                Ok(p) => p,
                Err(e) => return ToolOutcome::err(e),
            };
            let models=cat.providers.iter().filter(|(pid,p)|crate::auth::credential_configured(pid,p.api_key.as_deref())).flat_map(|(pid,p)|p.models.iter().map(move |m|serde_json::json!({"id":format!("{pid}/{}",m.id),"levels":crate::models::thinking_levels(m,p)})));
            let models: Vec<_> = models
                .into_iter()
                .filter(|m| {
                    allowed.as_ref().is_none_or(|p| {
                        !p["enabled"].as_bool().unwrap_or(true)
                            || p["allowed"]
                                .as_array()
                                .is_some_and(|items| items.iter().any(|item| item["id"] == m["id"]))
                    })
                })
                .collect();
            ToolOutcome::ok(serde_json::to_string(&models).unwrap())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_namespacing_in_tool_names_is_stripped() {
        // Real calls sent `functions.read`, and the whole subagent call was lost
        // because the registry has no such tool.
        assert_eq!(normalise_tool_name("functions.read"), "read");
        assert_eq!(normalise_tool_name("functions.bash"), "bash");
    }

    #[test]
    fn cosmetic_tool_name_decorations_are_stripped() {
        assert_eq!(normalise_tool_name("read()"), "read");
        assert_eq!(normalise_tool_name("  write  "), "write");
        assert_eq!(normalise_tool_name("replace:"), "replace");
        // Bare names must come through untouched.
        assert_eq!(normalise_tool_name("anchor_grep"), "anchor_grep");
        assert_eq!(normalise_tool_name("subagent_models"), "subagent_models");
    }

    fn catalog(entries: &[(&str, &[&str])]) -> crate::models::ModelsFile {
        let mut providers = std::collections::BTreeMap::new();
        for (pid, models) in entries {
            let provider = crate::models::ProviderEntry {
                models: models
                    .iter()
                    .map(|m| crate::models::ModelEntry {
                        id: (*m).to_string(),
                        ..Default::default()
                    })
                    .collect(),
                base_url: None,
                api: None,
                // A literal key, not an env var reference: `credential_configured`
                // treats a non-empty literal as configured, so the suggestion tests
                // do not depend on this machine's credentials. An empty string would
                // resolve to None and hide every model from the hint.
                api_key: Some("test-key".into()),
                headers: None,
                compat: None,
            };
            providers.insert((*pid).to_string(), provider);
        }
        crate::models::ModelsFile { providers }
    }

    #[test]
    fn a_near_miss_model_surfaces_the_real_one() {
        // `gpt-5.2` and `sonnet` were both guessed by a model that had no way to
        // enumerate the catalog. The error has to name what exists.
        let cat = catalog(&[
            ("openai", &["gpt-5.2-codex", "gpt-6"]),
            ("anthropic", &["claude-sonnet-4-5"]),
        ]);
        let hint = suggest_models(&cat, "gpt-5.2");
        assert!(
            hint.contains("gpt-5.2-codex"),
            "the near miss should surface the real id: {hint}"
        );

        let hint = suggest_models(&cat, "sonnet");
        assert!(
            hint.contains("claude-sonnet-4-5"),
            "a bare family name should still find the model: {hint}"
        );
    }

    #[test]
    fn the_unknown_model_hint_always_says_how_to_list_them() {
        let cat = catalog(&[("openai", &["gpt-6"])]);
        for asked in ["zzz-nothing-like-it", "", "gpt-6"] {
            let hint = suggest_models(&cat, asked);
            assert!(
                hint.contains("subagent_models"),
                "every hint must name the authoritative list (asked {asked:?}): {hint}"
            );
        }
    }

    #[test]
    fn the_shapes_that_really_failed_all_find_a_candidate() {
        // The exact ids from the failure log, against an id shape this project
        // actually ships. Each must produce a candidate rather than the
        // "no resemblance" dead end that sent the model guessing again.
        let cat = catalog(&[
            ("Bailian", &["deepseek-v4.1-flash-1017", "qwen3.8-max"]),
            ("volcengine-plan", &["glm-5.3", "deepseek-v4-pro"]),
            ("openai-codex", &["gpt-5.5", "gpt-5.3-codex-spark"]),
        ]);
        for asked in [
            "gpt-5.2",             // version near miss
            "gpt-5",               // bare version
            "gpt-4o",              // wrong vendor
            "sonnet",              // family only
            "claude-sonnet-4.5",   // family + version
            "deepseek-v4.1-flash", // missing provider prefix / suffix
        ] {
            let hint = suggest_models(&cat, asked);
            assert!(
                // The real goal is that the caller gets a concrete id it can use in
                // the next call, whether or not the ranking found a close match.
                hint.lines().any(|l| l.trim_start().contains('/'))
                    || hint.contains("(none configured)"),
                "asked {asked:?} left nothing to try: {hint}"
            );
        }
    }

    #[test]
    fn the_hint_is_short_enough_to_read() {
        // Listing the whole catalog would be hundreds of lines and would not say
        // which model was meant; the cap is what keeps it actionable.
        let many: Vec<String> = (0..200).map(|i| format!("model-{i}")).collect();
        let refs: Vec<&str> = many.iter().map(String::as_str).collect();
        let cat = catalog(&[("volcengine", &refs)]);
        let hint = suggest_models(&cat, "model-7");
        let listed = hint.lines().filter(|l| l.starts_with("  ")).count();
        assert!(listed <= 8, "hint listed {listed} models, expected <= 8");
    }
}
