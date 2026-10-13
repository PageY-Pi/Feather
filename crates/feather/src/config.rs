//! feather configuration — data only (`~/.feather/config.redb`).
//! No runtime code loading: model/provider/theme choices are plain data.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// "auto", "faux", or a catalog provider id (e.g. "anthropic", "zai").
    pub provider: String,
    /// model id per provider, e.g. "claude-sonnet-4-5" / "gpt-4.1"
    pub model: String,
    /// optional base URL override (gateway/proxy)
    pub base_url: Option<String>,
    /// pi-style thinking level default ("off".."max"); None = off
    pub default_thinking_level: Option<String>,
    /// per-model thinking level overrides (model id → level)
    pub model_thinking_levels: Option<std::collections::BTreeMap<String, String>>,
    pub auto_resume_latest: bool,
    /// max tokens per assistant turn
    pub max_tokens: u32,
    pub temperature: Option<f32>,
    /// context window in tokens, drives compaction thresholds
    pub context_window: u64,
    pub compaction: CompactionToml,
    /// theme palette override (pure data — see feather-tui::theme)
    pub palette_override: Option<serde_json::Value>,
    /// model id used for compaction summaries (from pi's compaction-model.json)
    pub compaction_model: Option<String>,
    /// subagent settings
    pub subagent: SubagentSettings,
    /// bash tool
    pub bash: BashSettings,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct CompactionToml {
    pub enabled: bool,
    pub reserve_tokens: u64,
    pub keep_recent_tokens: u64,
    pub summary_tool_result_budget: usize,
    pub kept_tool_result_budget: usize,
    pub shrink_kept: bool,
    pub keep_last_turns_unshrunk: u64,
}

impl From<&CompactionToml> for feather_core::compaction::CompactionSettings {
    fn from(c: &CompactionToml) -> Self {
        Self {
            enabled: c.enabled,
            reserve_tokens: c.reserve_tokens,
            keep_recent_tokens: c.keep_recent_tokens,
            summary_tool_result_budget: c.summary_tool_result_budget,
            kept_tool_result_budget: c.kept_tool_result_budget,
            shrink_kept: c.shrink_kept,
            keep_last_turns_unshrunk: c.keep_last_turns_unshrunk,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SubagentSettings {
    pub enabled: bool,
    /// hard cap: one subagent per task, no nesting beyond depth 1
    pub max_depth: u32,
    pub max_tokens: u32,
    #[serde(default = "default_true")]
    pub use_policy: bool,
}

fn default_true() -> bool {
    true
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct BashSettings {
    pub enabled: bool,
    /// per-command wall-clock cap in seconds
    pub timeout_secs: u64,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            provider: "auto".into(),
            model: "claude-sonnet-4-5".into(),
            base_url: None,
            default_thinking_level: None,
            model_thinking_levels: None,
            auto_resume_latest: false,
            max_tokens: 8192,
            temperature: None,
            context_window: 200_000,
            compaction: CompactionToml {
                enabled: true,
                reserve_tokens: 16_384,
                keep_recent_tokens: 20_000,
                summary_tool_result_budget: 2_000,
                kept_tool_result_budget: 4_000,
                shrink_kept: true,
                keep_last_turns_unshrunk: 2,
            },
            palette_override: None,
            compaction_model: None,
            subagent: SubagentSettings {
                enabled: true,
                use_policy: true,
                max_depth: 1,
                max_tokens: 4096,
            },
            bash: BashSettings {
                enabled: true,
                timeout_secs: 120,
            },
        }
    }
}

/// FEATHER_HOME (default `~/.feather`).
pub fn feather_home() -> PathBuf {
    if let Ok(h) = std::env::var("FEATHER_HOME") {
        if !h.is_empty() {
            return PathBuf::from(h);
        }
    }
    dirs_or_cwd().join(".feather")
}

fn dirs_or_cwd() -> PathBuf {
    std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
}

/// pi-style project context: AGENTS.md from the feather home (global) and
/// from the working directory walking up to the filesystem root (project +
/// ancestors). Each file becomes one `<project_instructions path="...">`
/// section, exactly like pi's system prompt assembly. Missing files are
/// skipped; duplicates (same file via ancestor overlap) are dropped.
pub fn project_context(root: &std::path::Path, home: &std::path::Path) -> String {
    let mut files: Vec<std::path::PathBuf> = Vec::new();
    if home.join("AGENTS.md").is_file() {
        files.push(home.join("AGENTS.md"));
    }
    let mut dir = Some(root);
    while let Some(d) = dir {
        let f = d.join("AGENTS.md");
        if f.is_file() && !files.contains(&f) {
            files.push(f);
        }
        dir = d.parent();
    }
    let mut out = String::new();
    for f in files {
        if let Ok(content) = std::fs::read_to_string(&f) {
            if content.trim().is_empty() {
                continue;
            }
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&format!(
                "<project_instructions path=\"{}\">\n{}\n</project_instructions>",
                f.display(),
                content.trim_end()
            ));
        }
    }
    out
}
pub fn cache_settings(
    cwd: &std::path::Path,
    home: &std::path::Path,
) -> feather_cache::CacheSettings {
    let prefs = crate::preferences::load_preferences(cwd, home, true);
    let mut settings = feather_cache::CacheSettings::default();
    if let Some(v) = prefs.effective.get("cacheOptimizer") {
        if let Some(n) = v["minBreakpointTokens"].as_u64() {
            settings.min_breakpoint_tokens = n;
        }
        if let Some(n) = v["stickyTtlSeconds"].as_u64() {
            settings.sticky_ttl_seconds = Some(n);
        }
        if let Some(b) = v["cacheSystem"].as_bool() {
            settings.cache_system = b;
        }
        if let Some(b) = v["cacheTools"].as_bool() {
            settings.cache_tools = b;
        }
    }
    settings
}

/// Resolve resource lists in their own settings-file scope.
pub fn resources_for(
    cwd: &std::path::Path,
    home: &std::path::Path,
) -> crate::resources::ResourceLoad {
    let prefs = crate::preferences::load_preferences(cwd, home, true);
    let paths = |key: &str| {
        let mut out = Vec::new();
        for (settings, base) in [
            (&prefs.global, home.to_path_buf()),
            (&prefs.project, cwd.join(".feather")),
        ] {
            if let Some(entries) = settings.get(key).and_then(serde_json::Value::as_array) {
                for entry in entries.iter().filter_map(serde_json::Value::as_str) {
                    let (prefix, path) = if let Some(p) = entry.strip_prefix('+') {
                        ("+", p)
                    } else if let Some(p) = entry.strip_prefix('-') {
                        ("-", p)
                    } else {
                        ("", entry)
                    };
                    if entry.starts_with('!')
                        || path.starts_with("~/")
                        || std::path::Path::new(path).is_absolute()
                    {
                        out.push(entry.to_string());
                    } else {
                        out.push(format!("{prefix}{}", base.join(path).display()));
                    }
                }
            }
        }
        out
    };
    crate::resources::load_resources(&crate::resources::ResourceOptions {
        cwd: cwd.to_path_buf(),
        feather_dir: home.to_path_buf(),
        skill_paths: paths("skills"),
        prompt_paths: paths("prompts"),
        include_defaults: true,
    })
}

impl Settings {
    /// Persist the thinking level (pi's setDefaultThinkingLevel): writes the
    /// settings file, preserving other fields.
    pub fn save_thinking_level(&self, model: &str, level: &str) -> std::io::Result<()> {
        crate::config_store::update(&feather_home(), "settings", |value| {
            value["default_thinking_level"] = serde_json::json!(level);
            value["defaultThinkingLevel"] = serde_json::json!(level);
            if !value["model_thinking_levels"].is_object() {
                value["model_thinking_levels"] = serde_json::json!({});
            }
            value["model_thinking_levels"][model] = serde_json::json!(level);
            value["modelThinkingLevels"] = value["model_thinking_levels"].clone();
        })
    }

    pub fn save(&self) -> std::io::Result<()> {
        let mut patch = serde_json::to_value(self).map_err(std::io::Error::other)?;
        patch["defaultModel"] = serde_json::json!(self.model);
        patch["defaultProvider"] = serde_json::json!(self.provider);
        patch["defaultThinkingLevel"] = serde_json::json!(self.default_thinking_level);
        patch["modelThinkingLevels"] = serde_json::json!(self.model_thinking_levels);
        patch["autoResumeLatest"] = serde_json::json!(self.auto_resume_latest);
        patch["maxTokens"] = serde_json::json!(self.max_tokens);
        patch["contextWindow"] = serde_json::json!(self.context_window);
        patch["baseUrl"] = serde_json::json!(self.base_url);
        for (snake, camel) in [
            ("reserve_tokens", "reserveTokens"),
            ("keep_recent_tokens", "keepRecentTokens"),
            ("summary_tool_result_budget", "summaryToolResultBudget"),
            ("kept_tool_result_budget", "keptToolResultBudget"),
            ("shrink_kept", "shrinkKept"),
            ("keep_last_turns_unshrunk", "keepLastTurnsUnshrunk"),
        ] {
            patch["compaction"][camel] = patch["compaction"][snake].clone();
        }
        crate::config_store::update(&feather_home(), "settings", |v| {
            *v = crate::preferences::deep_merge_objects(v, &patch);
        })
    }
    pub fn load() -> Self {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        Self::load_for(&cwd, &feather_home())
    }

    pub fn load_for(cwd: &std::path::Path, home: &std::path::Path) -> Self {
        let preferences = crate::preferences::load_preferences(cwd, home, true);
        Self::from_value(preferences.effective)
    }

    pub fn from_value(mut value: serde_json::Value) -> Self {
        // Keep the original Feather schema and accept compatible scoped settings.
        for (camel, snake) in [
            ("defaultProvider", "provider"),
            ("defaultModel", "model"),
            ("defaultThinkingLevel", "default_thinking_level"),
            ("modelThinkingLevels", "model_thinking_levels"),
            ("autoResumeLatest", "auto_resume_latest"),
            ("maxTokens", "max_tokens"),
            ("contextWindow", "context_window"),
            ("baseUrl", "base_url"),
        ] {
            if let Some(v) = value.get(camel).cloned() {
                value[snake] = v;
            }
        }
        if let Some(c) = value.get_mut("compaction") {
            for (camel, snake) in [
                ("reserveTokens", "reserve_tokens"),
                ("keepRecentTokens", "keep_recent_tokens"),
            ] {
                if let Some(v) = c.get(camel).cloned() {
                    c[snake] = v;
                }
            }
        }
        serde_json::from_value(value).unwrap_or_default()
    }

    pub fn save_default_if_missing(&self) {
        if let Ok(Some(value)) = crate::config_store::read(&feather_home(), "settings") {
            if value.as_object().map(|v| v.is_empty()).unwrap_or(false) {
                let _ = self.save();
            }
        }
    }
}

#[cfg(test)]
mod context_tests {
    use super::*;

    fn temp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "feather-ctx-{tag}-{}-{}",
            std::process::id(),
            feather_core::message::now_millis()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn project_context_reads_home_and_ancestor_agents_md() {
        let home = temp("home");
        let proj = temp("proj").join("sub");
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::write(home.join("AGENTS.md"), "global rules").unwrap();
        std::fs::write(proj.parent().unwrap().join("AGENTS.md"), "project rules").unwrap();
        let ctx = project_context(&proj, &home);
        assert!(ctx.contains("<project_instructions"), "{ctx}");
        assert!(ctx.contains("global rules"));
        assert!(ctx.contains("project rules"));
        // ancestor-first ordering: home (global) comes before the project file
        let g = ctx.find("global rules").unwrap();
        let p = ctx.find("project rules").unwrap();
        assert!(g < p);
        let _ = std::fs::remove_dir_all(&home);
        let _ = std::fs::remove_dir_all(proj.parent().unwrap());
    }

    #[test]
    fn project_context_is_empty_without_agents_md() {
        let home = temp("empty-home");
        let proj = temp("empty-proj");
        assert_eq!(project_context(&proj, &home), "");
        let _ = std::fs::remove_dir_all(&home);
        let _ = std::fs::remove_dir_all(&proj);
    }
}
