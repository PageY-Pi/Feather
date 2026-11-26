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
    /// Images the model should see, as `(media_type, base64)` pairs.
    ///
    /// Empty for every existing tool, so this is purely additive. A tool that reads a
    /// picture puts it here rather than in `text`, because base64 in a text block is
    /// charged as text and tells the model nothing about the pixels.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<(String, String)>,
}

impl ToolOutcome {
    pub fn ok(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            is_error: false,
            details: None,
            images: Vec::new(),
        }
    }

    pub fn err(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            is_error: true,
            details: None,
            images: Vec::new(),
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

    /// The error for a tool name this registry does not know.
    ///
    /// A model can put prose where the tool name belongs. Observed in the wild:
    /// a deepseek model emitted its native `<|DSML|> invoke name=` syntax inside
    /// the *content* stream, and the name field ended up holding an entire
    /// multi-line note followed by the marker — so `name` was ~350 characters of
    /// Chinese prose plus `…<｜｜DSML｜｜ invoke name=`. Echoing that back verbatim
    /// teaches the model nothing and burns context on every retry.
    ///
    /// So the reply is bounded and actionable: what arrived (short), whether it
    /// looks like a leaked tool-call marker rather than a real name, and the
    /// nearest real tool name when the input resembles one. `execute_progress`
    /// and `ToolRegistry::run` both go through here so the two paths cannot drift.
    pub fn unknown_tool_error(&self, name: &str) -> String {
        // Control characters and newlines are the tell: a real tool name is a
        // short bare identifier. Clamp the echo so a note-sized name costs a
        // line, not a screen.
        let looks_leaked = name.chars().any(|c| c.is_control() || c == '\n')
            || name.contains("DSML")
            || name.contains("<|")
            || name.chars().count() > 40;
        let echo: String = if looks_leaked {
            // Only echo the part that can plausibly be a tool name: a leading run of
            // identifier characters. Anything else is prose, and prose is exactly
            // what must not come back — a 40-char clamp still reproduced
            // `constraints> - 不做 defensive writing` whole.
            let ident: String = name
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
                .collect();
            let prefix: String = ident.chars().take(40).collect();
            if prefix.is_empty() {
                format!("(no name; {} chars of text)", name.chars().count())
            } else {
                format!("{prefix}… ({} chars)", name.chars().count())
            }
        } else {
            name.to_string()
        };
        let mut msg = format!("unknown tool: {echo}");
        if looks_leaked {
            msg.push_str(
                ". This is not a tool name — it looks like prose or a leaked tool-call \
                 marker in the name field. Call a tool by its bare name.",
            );
        }
        if let Some(near) = self.nearest_name(name) {
            msg.push_str(&format!(" Did you mean `{near}`?"));
        }
        msg.push_str(&format!(" Available: {}.", self.names().join(", ")));
        msg
    }
    /// Closest registered name to `haystack`, by shared-token overlap.
    ///
    /// Deliberately crude: the goal is to rescue `grep` from a leaked marker or a
    /// typo, not to build an edit-distance index over sixteen strings. Returns
    /// `None` when nothing resembles the input, because a confident wrong guess
    /// is worse than no guess.
    fn nearest_name(&self, haystack: &str) -> Option<&'static str> {
        let lower = haystack.to_ascii_lowercase();
        let mut best: Option<(usize, &'static str)> = None;
        for candidate in self.tools.keys() {
            // Whole-name substring (the names are short and distinctive) or a
            // token-level match, which catches `usage of grep tool`.
            let hit = lower.contains(&candidate.to_ascii_lowercase());
            let score = if hit { candidate.len() + 10 } else { 0 };
            if score == 0 {
                continue;
            }
            if best.is_none_or(|(s, _)| score > s) {
                best = Some((score, candidate));
            }
        }
        best.map(|(_, n)| n)
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
            None => ToolOutcome::err(self.unknown_tool_error(&call.name)),
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

    /// The verbatim shape that reached the traffic report. A deepseek model put its
    /// whole note — plus a native `<｜｜DSML｜｜ invoke name=` marker — in the tool
    /// *name* field, so `name` was hundreds of characters of prose. Echoing that
    /// back teaches nothing and costs context on every retry.
    #[tokio::test]
    async fn a_leaked_marker_in_the_name_field_is_reported_without_dumping_the_note() {
        let mut reg = ToolRegistry::new();
        reg.add(std::sync::Arc::new(Echo));
        let leaked = "constraints>\n- 不做 defensive writing\n- 无分号、无 em dash\n- 不改数字\n\
                      </constraints>\n\n现在在 `submit_flat/paper5.tex` 上恢复身份信息，\
                      这是生成提交包的源文件。\n\n<｜｜DSML｜｜ calls>\n<｜｜DSML｜｜ invoke name=";
        let call = ToolCall {
            id: "1".into(),
            name: leaked.into(),
            arguments: serde_json::json!({}),
        };
        let (_, rx) = tokio::sync::watch::channel(false);
        let out = reg.execute_call(&call, rx).await;
        assert!(out.is_error);
        let text = out.text;
        // Bounded: the note must not be reproduced in full.
        assert!(
            text.chars().count() < 400,
            "the error must stay short, got {} chars",
            text.chars().count()
        );
        assert!(
            !text.contains("不做 defensive writing"),
            "the prose must not be echoed back: {text}"
        );
        // Actionable: it names the failure and lists real tools.
        assert!(text.contains("not a tool name"), "{text}");
        assert!(text.contains("echo"), "must list available tools: {text}");
    }

    /// A plain typo keeps its full echo (no truncation noise) and gets a pointer to
    /// the tool the model probably meant, rather than a bare "unknown tool".
    #[tokio::test]
    async fn a_typo_gets_the_nearest_real_name() {
        let mut reg = ToolRegistry::new();
        reg.add(std::sync::Arc::new(Echo));
        let call = ToolCall {
            id: "1".into(),
            name: "run echo".into(),
            arguments: serde_json::json!({}),
        };
        let (_, rx) = tokio::sync::watch::channel(false);
        let text = reg.execute_call(&call, rx).await.text;
        assert!(text.contains("unknown tool: run echo"), "{text}");
        // The name contains a registered tool, so the hint fires.
        assert!(text.contains("Did you mean `echo`?"), "{text}");
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
