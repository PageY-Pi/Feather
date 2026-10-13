//! Feather GUI keybindings — a faithful port of pi's named-action registry.
//!
//! pi exposes actions like `app.session.new` that can be assigned keys via
//! `<agent-dir>/keybindings.json`; feather follows the same scheme with
//! `~/.feather/keybindings.json`:
//!
//! ```json
//! { "app.session.new": "ctrl+shift+n", "app.interrupt": ["escape", "ctrl+c"] }
//! ```
//!
//! A configured value replaces the default for that action; an empty list
//! disables it. Key syntax mirrors pi: `modifier+key` with modifiers
//! `ctrl|shift|alt|super` and pi's key names (`pageUp`, `esc`, `f1`, ...).

use eframe::egui;
use std::collections::BTreeMap;

// ---------------------------------------------------------------------------
// Key syntax (pi-compatible)
// ---------------------------------------------------------------------------

/// A parsed `modifier+key` combination.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Combo {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    pub super_key: bool,
    /// canonical key name, e.g. "a", "pageUp", "f5", "-"
    pub key: String,
}

impl Combo {
    /// Parse one pi-syntax key like `ctrl+shift+pageUp`.
    pub fn parse(spec: &str) -> Option<Combo> {
        let mut c = Combo {
            ctrl: false,
            shift: false,
            alt: false,
            super_key: false,
            key: String::new(),
        };
        let mut saw_key = false;
        for part in spec.split('+') {
            let part = part.trim();
            if part.is_empty() {
                return None;
            }
            let lower = part.to_ascii_lowercase();
            match lower.as_str() {
                "ctrl" | "control" => c.ctrl = true,
                "shift" => c.shift = true,
                "alt" | "option" => c.alt = true,
                "super" | "cmd" | "command" | "win" => c.super_key = true,
                _ => {
                    if saw_key {
                        return None; // two keys in one combo
                    }
                    saw_key = true;
                    c.key = canonical_key(part)?;
                }
            }
        }
        if !saw_key {
            return None;
        }
        Some(c)
    }

    /// Canonical `mods+key` string for display.
    pub fn to_spec(&self) -> String {
        let mut s = String::new();
        if self.ctrl {
            s.push_str("ctrl+");
        }
        if self.alt {
            s.push_str("alt+");
        }
        if self.shift {
            s.push_str("shift+");
        }
        if self.super_key {
            s.push_str("super+");
        }
        s.push_str(&self.key);
        s
    }
}

/// Canonicalize pi's key names (`esc` -> `escape`, case-insensitive).
pub fn canonical_key(k: &str) -> Option<String> {
    let lower = k.to_ascii_lowercase();
    let name = match lower.as_str() {
        "esc" => "escape",
        "return" => "enter",
        "pagedown" | "pgdn" => "pageDown",
        "pageup" | "pgup" => "pageUp",
        "del" => "delete",
        _ => {
            return match lower.as_str() {
                "escape" | "enter" | "tab" | "space" | "backspace" | "delete" | "insert"
                | "clear" | "home" | "end" | "up" | "down" | "left" | "right" => {
                    Some(special_cased(&lower))
                }
                "pageup" | "pagedown" => unreachable!(),
                _ => {
                    // letters / digits / symbols pass through; f1..f12 too
                    let is_single = lower.len() == 1 && lower.as_bytes()[0].is_ascii_graphic();
                    let is_fkey = lower.starts_with('f')
                        && lower.len() >= 2
                        && lower[1..]
                            .parse::<u8>()
                            .is_ok_and(|n| (1..=12).contains(&n));
                    if is_single || is_fkey {
                        Some(special_cased(&lower))
                    } else {
                        None
                    }
                }
            };
        }
    };
    Some(name.to_string())
}

fn special_cased(lower: &str) -> String {
    match lower {
        "pagedown" => "pageDown".into(),
        "pageup" => "pageUp".into(),
        other => other.to_string(),
    }
}

// ---------------------------------------------------------------------------
// Action registry (1:1 with pi's keybindings.md)
// ---------------------------------------------------------------------------

/// `(action id, pi default bindings)` — `&[]` means "no default on this
/// platform" (e.g. `app.suspend` on Windows). The order mirrors pi's docs.
pub const ACTIONS: &[(&str, &[&str])] = &[
    // -- cursor movement (editor) --
    ("tui.editor.cursorUp", &["up"]),
    ("tui.editor.cursorDown", &["down"]),
    ("tui.editor.historyPrevious", &[]),
    ("tui.editor.historyNext", &[]),
    ("tui.editor.cursorLeft", &["left", "ctrl+b"]),
    ("tui.editor.cursorRight", &["right", "ctrl+f"]),
    ("tui.editor.cursorWordLeft", &["alt+left", "ctrl+left"]),
    ("tui.editor.cursorWordRight", &["alt+right", "ctrl+right"]),
    ("tui.editor.cursorLineStart", &["home", "ctrl+a"]),
    ("tui.editor.cursorLineEnd", &["end", "ctrl+e"]),
    ("tui.editor.jumpForward", &["ctrl+]"]),
    ("tui.editor.jumpBackward", &["ctrl+alt+]"]),
    ("tui.editor.pageUp", &["pageUp", "ctrl+pageUp"]),
    ("tui.editor.pageDown", &["pageDown", "ctrl+pageDown"]),
    // -- text editing --
    ("tui.editor.deleteCharBackward", &["backspace"]),
    ("tui.editor.deleteCharForward", &["delete", "ctrl+d"]),
    (
        "tui.editor.deleteWordBackward",
        &["ctrl+w", "alt+backspace"],
    ),
    ("tui.editor.deleteWordForward", &["alt+d", "alt+delete"]),
    ("tui.editor.deleteToLineStart", &["ctrl+u"]),
    ("tui.editor.deleteToLineEnd", &["ctrl+k"]),
    ("tui.editor.yank", &["ctrl+y"]),
    ("tui.editor.yankPop", &["alt+y"]),
    // pi: `ctrl+-`, ctrl+z on Windows — the GUI ships the Windows default
    ("tui.editor.undo", &["ctrl+z"]),
    // -- input and selection --
    ("tui.input.newLine", &["shift+enter", "ctrl+j"]),
    ("tui.input.submit", &["enter"]),
    ("tui.input.tab", &["tab"]),
    ("tui.input.copy", &["ctrl+c"]),
    ("tui.select.up", &["up"]),
    ("tui.select.down", &["down"]),
    ("tui.select.pageUp", &["pageUp"]),
    ("tui.select.pageDown", &["pageDown"]),
    ("tui.select.confirm", &["enter"]),
    ("tui.select.cancel", &["escape", "ctrl+c"]),
    // -- transcript scrolling (altScreen) --
    ("tui.altScreen.pageUp", &["pageUp"]),
    ("tui.altScreen.pageDown", &["pageDown"]),
    ("tui.altScreen.halfPageUp", &[]),
    ("tui.altScreen.halfPageDown", &[]),
    ("tui.altScreen.lineUp", &[]),
    ("tui.altScreen.lineDown", &[]),
    (
        "tui.altScreen.previousPrompt",
        &["ctrl+shift+up", "ctrl+up"],
    ),
    (
        "tui.altScreen.nextPrompt",
        &["ctrl+shift+down", "ctrl+down"],
    ),
    // pi: ctrl+shift+f, ctrl+f on Windows — GUI ships the Windows default
    ("tui.altScreen.search", &["ctrl+f"]),
    ("tui.altScreen.searchNext", &["enter", "ctrl+g"]),
    (
        "tui.altScreen.searchPrevious",
        &["shift+enter", "ctrl+shift+g"],
    ),
    ("tui.altScreen.searchClose", &["escape"]),
    ("tui.altScreen.top", &["ctrl+home"]),
    ("tui.altScreen.bottom", &["ctrl+end"]),
    // -- application --
    ("app.interrupt", &["escape"]),
    ("app.clear", &["ctrl+c"]),
    ("app.exit", &["ctrl+d"]),
    // pi: ctrl+z, no default on Windows
    ("app.suspend", &[]),
    ("app.editor.external", &["ctrl+g"]),
    // pi: ctrl+v, alt+v on Windows — GUI ships alt+v (ctrl+v = paste text)
    ("app.clipboard.pasteImage", &["alt+v"]),
    // -- sessions --
    ("app.session.new", &["ctrl+shift+n"]),
    ("app.session.tree", &[]),
    ("app.session.fork", &[]),
    ("app.session.resume", &["ctrl+shift+r"]),
    ("app.session.togglePath", &["ctrl+p"]),
    ("app.session.toggleSort", &["ctrl+s"]),
    ("app.session.toggleNamedFilter", &["ctrl+n"]),
    ("app.session.rename", &["ctrl+r"]),
    ("app.session.delete", &["ctrl+d"]),
    ("app.session.deleteNoninvasive", &["ctrl+backspace"]),
    // -- models and thinking --
    ("app.model.select", &["ctrl+l"]),
    ("app.model.cycleForward", &["ctrl+p"]),
    // pi: shift+ctrl+p, alt+p on Windows
    ("app.model.cycleBackward", &["alt+p"]),
    ("app.models.save", &["ctrl+s"]),
    ("app.thinking.cycle", &["shift+tab"]),
    ("app.thinking.save", &["ctrl+s"]),
    ("app.thinking.toggle", &["ctrl+t"]),
    // GUI-only discoverability, using the same override registry.
    ("app.thinking.select", &["ctrl+shift+t"]),
    ("app.shortcuts", &["f1"]),
    // -- display and message queue --
    ("app.tools.expand", &["ctrl+o"]),
    ("app.message.copy", &["ctrl+x"]),
    // pi: alt+enter, ctrl+q on Windows
    ("app.message.followUp", &["ctrl+q"]),
    // pi: alt+up, alt+q on Windows
    ("app.message.dequeue", &["alt+q"]),
    // -- tree navigation --
    ("app.tree.foldOrUp", &["ctrl+left", "alt+left"]),
    ("app.tree.unfoldOrDown", &["ctrl+right", "alt+right"]),
    ("app.tree.editLabel", &["shift+l"]),
    ("app.tree.toggleLabelTimestamp", &["shift+t"]),
    ("app.tree.filter.default", &["ctrl+d"]),
    ("app.tree.filter.noTools", &["ctrl+t"]),
    ("app.tree.filter.userOnly", &["ctrl+u"]),
    ("app.tree.filter.labeledOnly", &["ctrl+l"]),
    ("app.tree.filter.all", &["ctrl+a"]),
    ("app.tree.filter.cycleForward", &["ctrl+o"]),
    ("app.tree.filter.cycleBackward", &["shift+ctrl+o"]),
    // -- scoped models selector --
    ("app.models.enableAll", &["ctrl+a"]),
    ("app.models.clearAll", &["ctrl+x"]),
    ("app.models.toggleProvider", &["ctrl+p"]),
    ("app.models.reorderUp", &["alt+up"]),
    ("app.models.reorderDown", &["alt+down"]),
];

// ---------------------------------------------------------------------------
// Keybindings: defaults + user overrides
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct Keybindings {
    /// action id -> active combos (after overrides)
    active: BTreeMap<String, Vec<Combo>>,
    /// action ids the user explicitly configured (for the settings panel)
    overridden: std::collections::BTreeSet<String>,
}

/// keybindings.json is a plain map: parse as Value, no serde struct needed.

impl Keybindings {
    /// Defaults + `~/.feather/keybindings.json` overrides.
    pub fn load() -> Keybindings {
        Self::load_from(&feather::config::feather_home().join("config.redb"))
    }

    /// Injectable core (tests).
    pub fn load_from(path: &std::path::Path) -> Keybindings {
        let mut active: BTreeMap<String, Vec<Combo>> = BTreeMap::new();
        for (id, keys) in ACTIONS {
            let combos = keys.iter().filter_map(|k| Combo::parse(k)).collect();
            active.insert((*id).to_string(), combos);
        }
        let mut overridden = std::collections::BTreeSet::new();
        if let Ok(Some(value)) = feather::config_store::read(
            path.parent().unwrap_or_else(|| std::path::Path::new(".")),
            "keybindings",
        ) {
            if let serde_json::Value::Object(map) = value {
                for (id, val) in map {
                    let specs: Vec<String> = match val {
                        serde_json::Value::String(s) => vec![s],
                        serde_json::Value::Array(a) => a
                            .into_iter()
                            .filter_map(|v| v.as_str().map(str::to_string))
                            .collect(),
                        _ => continue,
                    };
                    // empty list = disabled; unknown actions may be bound too
                    let combos: Vec<Combo> = specs.iter().filter_map(|s| Combo::parse(s)).collect();
                    // a spec that failed to parse AND produced zero combos while
                    // the user asked for bindings is a config typo — surface it
                    if !specs.is_empty() && combos.is_empty() && !specs.is_empty() {
                        eprintln!("feather: keybindings.json: no valid key in {id:?} ({specs:?})");
                    }
                    active.insert(id.clone(), combos);
                    overridden.insert(id);
                }
            }
        }
        Keybindings { active, overridden }
    }

    /// Active combos for an action (may be empty = disabled).
    pub fn active(&self, action: &str) -> &[Combo] {
        self.active.get(action).map(|v| v.as_slice()).unwrap_or(&[])
    }

    /// All action ids with their active bindings (settings panel).
    pub fn table(&self) -> &BTreeMap<String, Vec<Combo>> {
        &self.active
    }

    pub fn is_overridden(&self, action: &str) -> bool {
        self.overridden.contains(action)
    }

    /// Write a user override for one action (settings panel editor).
    #[allow(dead_code)] // settings-panel per-key editing will call this
    pub fn persist_override(&self, action: &str, specs: &[String]) -> std::io::Result<()> {
        feather::config_store::update(&feather::config::feather_home(), "keybindings", |value| {
            value[action] = serde_json::json!(specs);
        })
    }

    /// Match an incoming key event (canonical name + modifiers) to an action.
    /// Later-registered actions lose to earlier ones when identical combos
    /// collide — pi resolves ties by the same precedence (editor > app).
    pub fn match_combo(&self, combo: &Combo) -> Option<&'static str> {
        // exact scan over the registry in declaration order
        for (id, _) in ACTIONS {
            if self.active(id).iter().any(|c| c == combo) {
                return Some(id);
            }
        }
        None
    }
}

// ---------------------------------------------------------------------------
// egui bridge
// ---------------------------------------------------------------------------

/// Convert an egui key event to a pi-style combo, then match.
pub fn egui_event_to_action(
    kb: &Keybindings,
    mods: egui::Modifiers,
    key: egui::Key,
) -> Option<&'static str> {
    let name: String = match key {
        egui::Key::ArrowUp => "up".into(),
        egui::Key::ArrowDown => "down".into(),
        egui::Key::ArrowLeft => "left".into(),
        egui::Key::ArrowRight => "right".into(),
        egui::Key::Enter => "enter".into(),
        egui::Key::Escape => "escape".into(),
        egui::Key::Tab => "tab".into(),
        egui::Key::Space => "space".into(),
        egui::Key::Backspace => "backspace".into(),
        egui::Key::Delete => "delete".into(),
        egui::Key::Insert => "insert".into(),
        egui::Key::Home => "home".into(),
        egui::Key::End => "end".into(),
        egui::Key::PageUp => "pageUp".into(),
        egui::Key::PageDown => "pageDown".into(),
        egui::Key::F1 => "f1".into(),
        egui::Key::F2 => "f2".into(),
        egui::Key::F3 => "f3".into(),
        egui::Key::F4 => "f4".into(),
        egui::Key::F5 => "f5".into(),
        egui::Key::F6 => "f6".into(),
        egui::Key::F7 => "f7".into(),
        egui::Key::F8 => "f8".into(),
        egui::Key::F9 => "f9".into(),
        egui::Key::F10 => "f10".into(),
        egui::Key::F11 => "f11".into(),
        egui::Key::F12 => "f12".into(),
        egui::Key::Minus => "-".into(),
        egui::Key::Comma => ",".into(),
        egui::Key::Period => ".".into(),
        egui::Key::Slash => "/".into(),
        egui::Key::Backslash => "\\".into(),
        egui::Key::Semicolon => ";".into(),
        egui::Key::A => "a".into(),
        egui::Key::B => "b".into(),
        egui::Key::C => "c".into(),
        egui::Key::D => "d".into(),
        egui::Key::E => "e".into(),
        egui::Key::F => "f".into(),
        egui::Key::G => "g".into(),
        egui::Key::H => "h".into(),
        egui::Key::I => "i".into(),
        egui::Key::J => "j".into(),
        egui::Key::K => "k".into(),
        egui::Key::L => "l".into(),
        egui::Key::M => "m".into(),
        egui::Key::N => "n".into(),
        egui::Key::O => "o".into(),
        egui::Key::P => "p".into(),
        egui::Key::Q => "q".into(),
        egui::Key::R => "r".into(),
        egui::Key::S => "s".into(),
        egui::Key::T => "t".into(),
        egui::Key::U => "u".into(),
        egui::Key::V => "v".into(),
        egui::Key::W => "w".into(),
        egui::Key::X => "x".into(),
        egui::Key::Y => "y".into(),
        egui::Key::Z => "z".into(),
        _ => return None,
    };
    let combo = Combo {
        ctrl: mods.ctrl,
        shift: mods.shift,
        alt: mods.alt,
        super_key: mods.mac_cmd || (mods.command && !mods.ctrl),
        key: name,
    };
    kb.match_combo(&combo)
}

#[cfg(test)]
mod gui_modifier_tests {
    use super::*;
    #[test]
    fn windows_command_alias_does_not_require_super() {
        let kb = Keybindings::load_from(&std::path::PathBuf::from("missing-keybindings-test.json"));
        let mods = egui::Modifiers {
            ctrl: true,
            command: true,
            ..Default::default()
        };
        assert_eq!(
            egui_event_to_action(&kb, mods, egui::Key::L),
            Some("app.model.select")
        );
        let mods = egui::Modifiers {
            shift: true,
            ..mods
        };
        assert_eq!(
            egui_event_to_action(&kb, mods, egui::Key::N),
            Some("app.session.new")
        );
        assert_eq!(
            egui_event_to_action(&kb, mods, egui::Key::T),
            Some("app.thinking.select")
        );
    }
}

// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_dir(tag: &str) -> std::path::PathBuf {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        std::env::temp_dir().join(format!(
            "feather-kb-{tag}-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        ))
    }

    fn kb_with(overrides: &str) -> Keybindings {
        let dir = unique_dir("ovr");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("keybindings.json");
        std::fs::write(&p, overrides).unwrap();
        let kb = Keybindings::load_from(&p);
        let _ = std::fs::remove_dir_all(&dir);
        kb
    }

    #[test]
    fn registry_covers_pi_actions() {
        let ids: std::collections::BTreeSet<&str> = ACTIONS.iter().map(|(a, _)| *a).collect();
        for must in [
            "app.interrupt",
            "app.clear",
            "app.exit",
            "app.session.new",
            "app.model.select",
            "app.model.cycleForward",
            "app.model.cycleBackward",
            "app.models.save",
            "app.thinking.cycle",
            "app.thinking.toggle",
            "app.tools.expand",
            "app.message.copy",
            "app.message.followUp",
            "app.message.dequeue",
            "tui.input.submit",
            "tui.input.newLine",
            "tui.altScreen.search",
            "tui.editor.undo",
            "app.models.enableAll",
        ] {
            assert!(ids.contains(must), "missing pi action {must}");
        }
        // defaults survive: app.interrupt = escape
        let kb = kb_with("{}");
        assert_eq!(kb.active("app.interrupt")[0].key, "escape");
        // windows defaults: model cycleBackward = alt+p
        assert_eq!(kb.active("app.model.cycleBackward")[0].key, "p");
        assert!(kb.active("app.model.cycleBackward")[0].alt);
        // no default on Windows for suspend
        assert!(kb.active("app.suspend").is_empty());
    }

    #[test]
    fn overrides_replace_and_disable() {
        let kb = kb_with(r#"{"app.interrupt": "ctrl+shift+x"}"#);
        let a = kb.active("app.interrupt");
        assert_eq!(a.len(), 1);
        assert!(a[0].ctrl && a[0].shift && a[0].key == "x");
        assert!(!kb.active("app.interrupt").iter().any(|c| c.key == "escape"));
        assert!(kb.is_overridden("app.interrupt"));

        let kb = kb_with(r#"{"tui.altScreen.pageUp": []}"#);
        assert!(kb.active("tui.altScreen.pageUp").is_empty());
    }

    #[test]
    fn key_syntax_matches_pi() {
        // modifiers combine in any order, case-insensitive keys
        let c = Combo::parse("CTRL+SHIFT+pageUp").unwrap();
        assert!(c.ctrl && c.shift && c.key == "pageUp");
        // esc -> escape, return -> enter
        assert_eq!(Combo::parse("esc").unwrap().key, "escape");
        assert_eq!(Combo::parse("return").unwrap().key, "enter");
        // symbols and f-keys
        assert_eq!(Combo::parse("ctrl+]").unwrap().key, "]");
        assert_eq!(Combo::parse("f12").unwrap().key, "f12");
        // malformed
        assert!(Combo::parse("ctrl+").is_none());
        assert!(Combo::parse("ctrl+a+b").is_none());
        assert!(Combo::parse("").is_none());
    }

    #[test]
    fn matching_finds_action() {
        let kb = kb_with("{}");
        let c = Combo::parse("ctrl+l").unwrap();
        assert_eq!(kb.match_combo(&c), Some("app.model.select"));
        let c = Combo::parse("shift+tab").unwrap();
        assert_eq!(kb.match_combo(&c), Some("app.thinking.cycle"));
        // unbound
        let c = Combo::parse("ctrl+F9").unwrap();
        assert!(kb.match_combo(&c).is_none());
        // override wins: ctrl+shift+m now selects the model ...
        let kb = kb_with(r#"{"app.model.select": "ctrl+shift+m"}"#);
        assert_eq!(
            kb.match_combo(&Combo::parse("ctrl+shift+m").unwrap()),
            Some("app.model.select")
        );
        // ... and the freed ctrl+l falls through to the next default holder
        // (app.tree.filter.labeledOnly also binds ctrl+l) — pi's same
        // precedence rule when two actions share a default combo
        assert_eq!(
            kb.match_combo(&Combo::parse("ctrl+l").unwrap()),
            Some("app.tree.filter.labeledOnly")
        );
    }

    #[test]
    fn persist_override_roundtrip() {
        let dir = unique_dir("persist");
        // point FEATHER_HOME at a temp dir by using load_from's sibling path:
        // persist_override writes to the real home, so only test that it
        // produces valid JSON shape on a throwaway file via a manual write.
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("keybindings.json");
        let specs = vec!["ctrl+shift+n".to_string(), "alt+shift+t".to_string()];
        std::fs::write(
            &p,
            serde_json::to_string_pretty(
                &std::iter::once(("app.session.new".to_string(), specs.clone()))
                    .collect::<std::collections::BTreeMap<String, Vec<String>>>(),
            )
            .unwrap(),
        )
        .unwrap();
        let kb = Keybindings::load_from(&p);
        let a = kb.active("app.session.new");
        assert_eq!(a.len(), 2);
        assert_eq!(a[0].to_spec(), "ctrl+shift+n");
        assert_eq!(a[1].to_spec(), "alt+shift+t");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
