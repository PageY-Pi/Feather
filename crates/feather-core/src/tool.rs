//! Static tool registry.
//!
//! feather has **no runtime tool registration**: there are no extensions, no
//! script runtimes and no dynamic hooks. Tools are compiled in and assembled
//! by the binary crate into a `ToolRegistry`; the kernel only knows the
//! trait.

use crate::message::ToolCall;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;

/// What a tool returns to the model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolOutcome {
    pub text: String,
    #[serde(default)]
    pub is_error: bool,
    /// Structured side-channel: for the TUI/telemetry, never sent to the LLM.
    #[serde(default)]
    pub details: Option<serde_json::Value>,
}

impl ToolOutcome {
    pub fn ok(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            is_error: false,
            details: None,
        }
    }

    pub fn err(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            is_error: true,
            details: None,
        }
    }
}

pub type BoxFut<'a> = Pin<Box<dyn Future<Output = ToolOutcome> + Send + 'a>>;

/// A built-in coding tool.
pub trait AgentTool: Send + Sync {
    fn name(&self) -> &'static str;

    fn description(&self) -> &'static str;

    /// JSON Schema of the arguments object.
    fn parameters(&self) -> serde_json::Value;

    fn parallel_safe(&self) -> bool {
        false
    }

    fn execute_with_progress<'a>(
        &'a self,
        args: serde_json::Value,
        abort: tokio::sync::watch::Receiver<bool>,
        _progress: tokio::sync::mpsc::UnboundedSender<String>,
    ) -> BoxFut<'a> {
        self.execute(args, abort)
    }
    /// Run the tool with owned, parsed arguments plus an abort signal.
    /// Tools are async (bash spawns a process); boxed for dyn compatibility.
    fn execute<'a>(
        &'a self,
        args: serde_json::Value,
        abort: tokio::sync::watch::Receiver<bool>,
    ) -> BoxFut<'a>;
}

/// Display labels for tool rows, resolved by **exact tool name only**.
///
/// pi-omp-theme lesson: that theme renders collapsed tool panels through a
/// *shape* matcher, so `anchor_grep` (same `pattern`/`path` argument shape as
/// `grep`) was randomly relabeled "Grep:" in the collapsed state while the
/// expanded fallback showed "Anchor Grep". feather's rewrite keeps one
/// exact-name table as the single source of truth: a registered name always
/// wins, an unregistered name falls back to itself, and no argument shape can
/// ever override either.
pub fn tool_display_label(name: &str) -> &str {
    match name {
        "read" => "Read",
        "write" => "Write",
        "replace" => "Replace",
        "replace_within" => "Replace Within",
        "insert" => "Insert",
        "copy" => "Copy",
        "move" => "Move",
        "anchor_grep" => "Anchor Grep",
        "undo_last_change" => "Undo Last Change",
        other => other,
    }
}

/// Brace-free, human-readable rendering of tool-call arguments for tool cards
/// and transcript rows: `{"path":"a.rs","limit":3}` becomes `limit 3, path a.rs`
/// (keys render in sorted order — serde_json's map is BTreeMap-backed).
///
/// String values render bare (no quotes) with internal whitespace collapsed so
/// a multiline `command` still fits one row; other values (numbers, bools,
/// arrays, nested objects) keep their compact JSON form. Non-object arguments
/// render as the value itself. Output is capped at `max` chars with `…`.
pub fn tool_args_summary(args: &serde_json::Value, max: usize) -> String {
    fn value_to_text(v: &serde_json::Value) -> String {
        match v {
            serde_json::Value::String(s) => s.split_whitespace().collect::<Vec<_>>().join(" "),
            other => other.to_string(),
        }
    }
    let text = match args.as_object() {
        Some(map) => map
            .iter()
            .map(|(k, v)| format!("{k} {}", value_to_text(v)))
            .collect::<Vec<_>>()
            .join(", "),
        None => value_to_text(args),
    };
    if text.chars().count() <= max {
        text
    } else {
        let cut: String = text.chars().take(max).collect();
        format!("{cut}…")
    }
}

/// Compile-time tool set. Order is deterministic (BTreeMap by name) — one of
/// the cache-optimizer's stability invariants.
#[derive(Clone)]
pub struct ToolRegistry {
    tools: BTreeMap<&'static str, std::sync::Arc<dyn AgentTool>>,
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: BTreeMap::new(),
        }
    }

    pub fn add(&mut self, tool: std::sync::Arc<dyn AgentTool>) -> &mut Self {
        let name = tool.name(); // &'static str — the map key type
        if self.tools.insert(name, tool).is_some() {
            // a silent overwrite hides registration typos: two tools with the
            // same name means one silently stops existing
            debug_assert!(false, "duplicate tool name registration: {name}");
            eprintln!("feather: warning: duplicate tool name registration: {name}");
        }
        self
    }

    pub fn get(&self, name: &str) -> Option<std::sync::Arc<dyn AgentTool>> {
        self.tools.get(name).cloned()
    }

    pub fn names(&self) -> Vec<&'static str> {
        self.tools.keys().copied().collect()
    }

    pub fn defs(&self) -> Vec<crate::provider::ToolDef> {
        self.tools
            .values()
            .map(|t| crate::provider::ToolDef {
                name: t.name().to_string(),
                description: t.description().to_string(),
                parameters: t.parameters(),
            })
            .collect()
    }

    pub async fn execute_call(
        &self,
        call: &ToolCall,
        abort: tokio::sync::watch::Receiver<bool>,
    ) -> ToolOutcome {
        match self.get(&call.name) {
            Some(t) => {
                // normalize: arguments must be an object; wrap scalars
                let args = if call.arguments.is_object() {
                    call.arguments.clone()
                } else if call.arguments.is_null() {
                    serde_json::json!({})
                } else {
                    serde_json::json!({ "value": call.arguments })
                };
                t.execute(args, abort).await
            }
            None => ToolOutcome::err(format!("unknown tool: {}", call.name)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Echo;

    impl AgentTool for Echo {
        fn name(&self) -> &'static str {
            "echo"
        }
        fn description(&self) -> &'static str {
            "echo the input"
        }
        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({"type":"object","properties":{"text":{"type":"string"}}})
        }
        fn execute<'a>(
            &'a self,
            args: serde_json::Value,
            _abort: tokio::sync::watch::Receiver<bool>,
        ) -> BoxFut<'a> {
            Box::pin(async move {
                ToolOutcome::ok(
                    args.get("text")
                        .and_then(|t| t.as_str())
                        .unwrap_or("")
                        .to_string(),
                )
            })
        }
    }

    #[tokio::test]
    async fn registry_executes_by_name() {
        let mut reg = ToolRegistry::new();
        reg.add(std::sync::Arc::new(Echo));
        let (_tx, rx) = tokio::sync::watch::channel(false);
        let call = ToolCall {
            id: "1".into(),
            name: "echo".into(),
            arguments: serde_json::json!({"text":"hi"}),
        };
        let out = reg.execute_call(&call, rx).await;
        assert_eq!(out.text, "hi");
        let bad = ToolCall {
            id: "2".into(),
            name: "nope".into(),
            arguments: serde_json::json!({}),
        };
        let (_, rx2) = tokio::sync::watch::channel(false);
        assert!(reg.execute_call(&bad, rx2).await.is_error);
    }

    #[test]
    fn args_summary_strips_braces_and_quotes() {
        assert_eq!(
            tool_args_summary(&serde_json::json!({"path": "a.rs", "limit": 3}), 120),
            // keys render in sorted order (serde_json BTreeMap-backed map)
            "limit 3, path a.rs"
        );
        // multiline strings collapse to one line, no quotes
        assert_eq!(
            tool_args_summary(&serde_json::json!({"command": "ls -la\nfoo   bar"}), 120),
            "command ls -la foo bar"
        );
        // non-strings keep compact JSON; non-object args render as the value
        assert_eq!(
            tool_args_summary(&serde_json::json!({"ids": [1, 2]}), 120),
            "ids [1,2]"
        );
        assert_eq!(tool_args_summary(&serde_json::json!("plain"), 120), "plain");
        // overlong output is capped with an ellipsis
        let long = tool_args_summary(&serde_json::json!({"path": "x"}), 5);
        assert!(long.chars().count() <= 6 && long.ends_with('…'));
    }

    #[test]
    fn display_labels_resolve_by_exact_name_only() {
        // anchor_grep keeps its own label — the pi-omp-theme failure mode
        // (shape-matching it into the grep template) cannot recur
        assert_eq!(tool_display_label("anchor_grep"), "Anchor Grep");
        assert_eq!(tool_display_label("grep"), "grep");
        assert_eq!(tool_display_label("totally_unknown"), "totally_unknown");
    }
}
