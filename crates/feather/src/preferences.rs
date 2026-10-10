//! Pi-style settings, keybinding, system-prompt override, and resource-path data helpers.
//!
//! Pure data only: no TUI coupling and no mutation unless an explicit `save_*` API is called.

use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

const CONFIG_DIR_NAME: &str = ".feather";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Global,
    Project,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsPaths {
    pub global: PathBuf,
    pub project: PathBuf,
}
impl SettingsPaths {
    pub fn new(cwd: &Path, feather_dir: &Path) -> Self {
        Self {
            global: feather_dir.join("config.redb"),
            project: cwd.join(CONFIG_DIR_NAME).join("config.redb"),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Preferences {
    pub global: Value,
    pub project: Value,
    pub effective: Value,
    pub diagnostics: Vec<String>,
}

pub fn load_preferences(cwd: &Path, feather_dir: &Path, project_trusted: bool) -> Preferences {
    let paths = SettingsPaths::new(cwd, feather_dir);
    let mut d = Vec::new();
    let g = crate::config_store::read(feather_dir, "settings")
        .map(|v| v.unwrap_or_else(|| json!({})))
        .unwrap_or_else(|e| {
            d.push(format!("global settings: {e}"));
            json!({})
        });
    let p = if project_trusted {
        let project_home = paths.project.parent().unwrap_or(cwd);
        crate::config_store::read(project_home, "settings")
            .map(|v| v.unwrap_or_else(|| json!({})))
            .unwrap_or_else(|e| {
                d.push(format!("project settings: {e}"));
                json!({})
            })
    } else {
        json!({})
    };
    let g = migrate_settings(g);
    let p = migrate_settings(p);
    let effective = deep_merge_settings(&g, &p);
    Preferences {
        global: g,
        project: p,
        effective,
        diagnostics: d,
    }
}

pub fn migrate_settings(mut v: Value) -> Value {
    let Some(o) = v.as_object_mut() else {
        return json!({});
    };
    if o.contains_key("queueMode") && !o.contains_key("steeringMode") {
        if let Some(x) = o.remove("queueMode") {
            o.insert("steeringMode".into(), x);
        }
    }
    if !o.contains_key("transport") {
        if let Some(Value::Bool(b)) = o.remove("websockets") {
            o.insert(
                "transport".into(),
                json!(if b { "websocket" } else { "sse" }),
            );
        }
    }
    if let Some(sk) = o.get("skills").cloned() {
        if sk.is_object() {
            if let Some(b) = sk.get("enableSkillCommands") {
                if !o.contains_key("enableSkillCommands") {
                    o.insert("enableSkillCommands".into(), b.clone());
                }
            }
            if let Some(arr) = sk
                .get("customDirectories")
                .filter(|x| x.as_array().map(|a| !a.is_empty()).unwrap_or(false))
            {
                o.insert("skills".into(), arr.clone());
            } else {
                o.remove("skills");
            }
        }
    }
    // Feather backward compatibility: snake_case aliases feed pi-style camelCase if absent.
    alias(o, "provider", "defaultProvider");
    alias(o, "model", "defaultModel");
    alias(o, "default_thinking_level", "defaultThinkingLevel");
    alias(o, "model_thinking_levels", "modelThinkingLevels");
    if let Some(Value::Object(c)) = o.get_mut("compaction") {
        alias(c, "reserve_tokens", "reserveTokens");
        alias(c, "keep_recent_tokens", "keepRecentTokens");
        alias(c, "model_overrides", "modelOverrides");
    }
    v
}
fn alias(o: &mut Map<String, Value>, snake: &str, camel: &str) {
    if !o.contains_key(camel) {
        if let Some(v) = o.get(snake).cloned() {
            o.insert(camel.into(), v);
        }
    }
}
fn mergeable(v: &Value) -> bool {
    v.is_object()
}
pub fn deep_merge_settings(base: &Value, over: &Value) -> Value {
    let mut merged = deep_merge_objects(base, over);
    let base_dt = base.get("defaultTools");
    let over_dt = over.get("defaultTools");
    if let Some(o) = over_dt {
        if base_dt.and_then(Value::as_array).is_some()
            && o.as_array()
                .map(|a| a.iter().all(is_tool_modifier))
                .unwrap_or(false)
        {
            let mut a = base_dt.unwrap().as_array().unwrap().clone();
            a.extend(o.as_array().unwrap().clone());
            merged["defaultTools"] = Value::Array(a);
        }
    }
    merged
}
pub fn deep_merge_objects(base: &Value, over: &Value) -> Value {
    let (Some(b), Some(o)) = (base.as_object(), over.as_object()) else {
        return over.clone();
    };
    let mut r = b.clone();
    for (k, v) in o {
        let next = if r.get(k).map(mergeable).unwrap_or(false) && mergeable(v) {
            deep_merge_objects(r.get(k).unwrap(), v)
        } else {
            v.clone()
        };
        r.insert(k.clone(), next);
    }
    Value::Object(r)
}
fn is_tool_modifier(v: &Value) -> bool {
    v.as_str()
        .map(|s| s.starts_with('+') || s.starts_with('-'))
        .unwrap_or(false)
}
pub fn resolve_default_tools(entries: &[String]) -> Vec<String> {
    let plain: Vec<_> = entries
        .iter()
        .filter(|s| !s.starts_with('+') && !s.starts_with('-'))
        .cloned()
        .collect();
    let mut tools = if !plain.is_empty() || entries.is_empty() {
        plain
    } else {
        vec!["read".into(), "bash".into(), "edit".into(), "write".into()]
    };
    for e in entries {
        if let Some(n) = e.strip_prefix('+') {
            if !n.is_empty() && !tools.iter().any(|t| t == n) {
                tools.push(n.into())
            }
        } else if let Some(n) = e.strip_prefix('-') {
            tools.retain(|t| t != n);
        }
    }
    tools
}

/// Persist selected keys into the settings document of `path`'s redb store,
/// preserving unknown keys. Legacy `settings.json` is imported once by the store.
pub fn save_settings_patch(path: &Path, patch: &Value) -> io::Result<()> {
    if !patch.is_object() {
        return Err(io::Error::other("patch must be object"));
    }
    let home = path
        .parent()
        .ok_or_else(|| io::Error::other("settings path has no parent"))?;
    crate::config_store::update(home, "settings", |current| {
        *current = deep_merge_objects(current, patch);
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemPromptFiles {
    pub replacement: Option<PathBuf>,
    pub append: Option<PathBuf>,
}
pub fn discover_system_prompt_files(cwd: &Path, feather_dir: &Path) -> SystemPromptFiles {
    let project = cwd.join(CONFIG_DIR_NAME);
    let replacement = first_file(&[project.join("SYSTEM.md"), feather_dir.join("SYSTEM.md")]);
    let append = first_file(&[
        project.join("APPEND_SYSTEM.md"),
        feather_dir.join("APPEND_SYSTEM.md"),
    ]);
    SystemPromptFiles {
        replacement,
        append,
    }
}
fn first_file(paths: &[PathBuf]) -> Option<PathBuf> {
    paths.iter().find(|p| p.is_file()).cloned()
}
pub fn apply_system_prompt_override(
    default: &str,
    cwd: &Path,
    feather_dir: &Path,
) -> io::Result<String> {
    let f = discover_system_prompt_files(cwd, feather_dir);
    let mut s = if let Some(p) = f.replacement {
        fs::read_to_string(p)?
    } else {
        default.to_string()
    };
    if let Some(p) = f.append {
        let a = fs::read_to_string(p)?;
        if !s.ends_with('\n') {
            s.push('\n')
        }
        s.push_str(a.trim_end());
    }
    Ok(s)
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KeybindingConfig {
    pub bindings: BTreeMap<String, Vec<String>>,
    pub diagnostics: Vec<String>,
}
impl KeybindingConfig {
    pub fn load(path: &Path) -> Self {
        match crate::config_store::read(path.parent().unwrap_or_else(||Path::new(".")),"keybindings"){
            Ok(Some(Value::Object(o)))=>Self::from_object(o),Ok(None)=>Self::default(),
            Ok(Some(_))=>Self{diagnostics:vec!["keybindings redb document must be an object".into()],..Default::default()},
            Err(_)=>Self{diagnostics:vec!["keybindings redb read failed".into()],..Default::default()},
        }
    }
    pub fn from_object(o: Map<String, Value>) -> Self {
        let mut cfg = Self::default();
        for (action, v) in o {
            match v {
                Value::String(s) => cfg.bindings.insert(action, vec![s]),
                Value::Array(a) => cfg.bindings.insert(
                    action,
                    a.into_iter()
                        .filter_map(|x| x.as_str().map(str::to_string))
                        .collect(),
                ),
                _ => {
                    cfg.diagnostics
                        .push(format!("{action}: expected string or string[]"));
                    None
                }
            };
        }
        cfg
    }
    pub fn get_keys(&self, action: &str) -> Vec<String> {
        self.bindings.get(action).cloned().unwrap_or_default()
    }
    pub fn effective(
        defaults: &BTreeMap<String, Vec<String>>,
        user: &KeybindingConfig,
    ) -> BTreeMap<String, Vec<String>> {
        let mut m = defaults.clone();
        for (k, v) in &user.bindings {
            m.insert(k.clone(), v.clone());
        }
        m
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResourcePaths {
    pub skills: Vec<String>,
    pub prompts: Vec<String>,
    pub themes: Vec<String>,
    pub extensions: Vec<String>,
    pub enable_skill_commands: bool,
}
impl ResourcePaths {
    pub fn from_settings(v: &Value) -> Self {
        let arr = |k: &str| {
            v.get(k)
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default()
        };
        Self {
            skills: arr("skills"),
            prompts: arr("prompts"),
            themes: arr("themes"),
            extensions: arr("extensions"),
            enable_skill_commands: v
                .get("enableSkillCommands")
                .and_then(Value::as_bool)
                .unwrap_or(true),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn tmp(n: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("feather-pref-{n}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }
    #[test]
    fn merges_and_aliases_without_losing_unknown() {
        let g = migrate_settings(
            json!({"provider":"x","compaction":{"reserve_tokens":1},"unknown":1,"defaultTools":["+grep"]}),
        );
        assert_eq!(g["defaultProvider"], "x");
        assert_eq!(g["compaction"]["reserveTokens"], 1);
        let p = json!({"compaction":{"keepRecentTokens":2},"defaultTools":["-bash"]});
        let e = deep_merge_settings(&g, &p);
        assert_eq!(e["unknown"], 1);
        assert_eq!(e["compaction"]["reserveTokens"], 1);
        assert_eq!(e["defaultTools"].as_array().unwrap().len(), 2);
    }
    #[test]
    fn save_patch_preserves_unknown() {
        let d = tmp("save");
        // Legacy JSON is imported once into the temp config.redb, then patches merge.
        fs::write(d.join("settings.json"), "{\"unknown\":1,\"nested\":{\"a\":1}} ").unwrap();
        save_settings_patch(&d.join("config.redb"), &json!({"nested":{"b":2}})).unwrap();
        let v = crate::config_store::read(&d, "settings").unwrap().unwrap();
        assert_eq!(v["unknown"], 1);
        assert_eq!(v["nested"]["a"], 1);
        assert_eq!(v["nested"]["b"], 2);
        let _ = fs::remove_dir_all(d);
    }
    #[test]
    fn keybinding_data_replaces_defaults() {
        let mut d = BTreeMap::new();
        d.insert("a".into(), vec!["x".into()]);
        let cfg = KeybindingConfig::from_object(
            serde_json::from_value(json!({"a":[],"b":"ctrl+b"})).unwrap(),
        );
        let e = KeybindingConfig::effective(&d, &cfg);
        assert!(e["a"].is_empty());
        assert_eq!(e["b"], vec!["ctrl+b"]);
    }
}
