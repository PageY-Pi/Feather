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
fn policy() -> Result<Option<serde_json::Value>, String> {
    let home = crate::config::feather_home();
    crate::config_store::read(&home,POLICY_DOC).map(|value|value.filter(|v|!v.as_object().is_some_and(|m|m.is_empty()))).map_err(|_|"subagent policy redb read failed".into())
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
                return ToolOutcome::err(format!("subagent: unknown model {id}"));
            };
            if !crate::auth::credential_configured(pid, p.api_key.as_deref()) {
                return ToolOutcome::err("subagent: selected model has no credential");
            }
            provider = crate::wiring::provider_from_entry_for(pid, p, m);
            model = m.id.clone();
            context = m.context_window.unwrap_or(context);
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
        let anchors = Arc::new(AnchorStore::new(&anchor_home).with_snapshots(&self.session_dir, &self.root));
        let mut all = ToolRegistry::new();
        all.add(Arc::new(crate::web::WebFetchTool))
            .add(Arc::new(crate::web::WebSearchTool { model: requested.unwrap_or(&self.model).to_string(), base_url: None }));
        all.add(Arc::new(ReadTool {
            store: anchors.clone(),
        }))
        .add(Arc::new(WriteTool {
            store: anchors.clone(),
        }))
        .add(Arc::new(ReplaceTool {
            store: anchors.clone(),
        }))
        .add(Arc::new(ReplaceWithinTool {
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
                    out.push(n.into());
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
        if let Some(p) = &progress {
            let _ = p.send(format!("{label} · {id}\nStarting child"));
        }
        let mut timed_out = false;
        let mut persist_error = None;
        let result = loop {
            tokio::select! {
             result=&mut run=>break result,
             _=&mut deadline,if !timed_out=>{timed_out=true;let _=abort_tx.send(true);},
             changed=abort.changed(),if parent_open=>{if changed.is_err(){parent_open=false;}else if *abort.borrow(){let _=abort_tx.send(true);}},
             _=heartbeat.tick()=>{if let Some(p)=&progress{let _=p.send(format!("{label} · {id}\n{}",bounded(&partial,2048,40)));}},
             Ok(event)=events.recv()=>{match event{
              AgentEvent::AssistantMessage(message)=>{let _=message;partial.clear();},
              AgentEvent::TextDelta(text)=>{if partial.len()<2048{partial.push_str(&bounded(&text,2048-partial.len(),40));}},
              AgentEvent::ToolStart{call}=>{partial=format!("Running {}",call.name);},_=>{}
             }if last.elapsed()>=std::time::Duration::from_millis(100){if let Some(p)=&progress{let _=p.send(format!("{} · {}\n{}",label,id,bounded(&partial,2048,40)));}last=std::time::Instant::now();}}
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
        "Delegate an isolated task. Specify model, thinking, tools, cwd and timeoutMs; concurrent child calls run in parallel. Results are bounded; full child records are in redb."
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({"type":"object","properties":{"task":{"type":"string"},"label":{"type":"string"},"model":{"type":"string"},"thinking":{"type":"string","enum":["off","minimal","low","medium","high","xhigh","max"]},"tools":{"type":"array","items":{"type":"string"}},"cwd":{"type":"string"},"timeoutMs":{"type":"integer","minimum":1}},"required":["task"]})
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
