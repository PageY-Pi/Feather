//! Persistent renderer choice + restart handoff. Separate GUI-only config
//! document: choosing a renderer never overwrites model/auth/preferences.
use eframe::egui;
use crate::dialog::{choice_row, footnote, hint, sheet, Scrim};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Backend {
    #[default]
    Cpu,
    OpenGl,
    Wgpu,
}
impl Backend {
    pub fn status(self) -> &'static str {
        if self == Self::Cpu {
            "Render: CPU"
        } else {
            "Render: GPU"
        }
    }
    pub fn env(self) -> &'static str {
        match self {
            Self::Cpu => "cpu",
            Self::OpenGl => "glow",
            Self::Wgpu => "wgpu",
        }
    }
    pub fn from_env(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "cpu" | "software" => Some(Self::Cpu),
            "glow" | "opengl" => Some(Self::OpenGl),
            "wgpu" | "vulkan" | "dx12" => Some(Self::Wgpu),
            _ => None,
        }
    }
    /// Old saved OpenGL/"glow" selections migrate in memory to native wgpu
    /// on Windows. Never re-enter the WGL path sampled in the stalled process.
    pub fn safe_for_platform(self) -> Self {
        if cfg!(windows) && self == Self::OpenGl {
            Self::Wgpu
        } else {
            self
        }
    }
    pub fn accelerated() -> Self {
        if cfg!(windows) {
            Self::Wgpu
        } else {
            Self::OpenGl
        }
    }
}
pub fn load(home: &Path) -> Backend {
    feather::config_store::read(home, "gui")
        .ok()
        .flatten()
        .and_then(|v| v.get("renderer").cloned())
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default()
}
pub fn selected(home: &Path, preview: bool) -> Backend {
    std::env::var("FEATHER_GUI_RENDERER")
        .ok()
        .as_deref()
        .and_then(Backend::from_env)
        .unwrap_or_else(|| if preview { Backend::Cpu } else { load(home) })
        .safe_for_platform()
}
#[cfg(test)]
pub fn save(home: &Path, backend: Backend) -> std::io::Result<()> {
    feather::config_store::update(home, "gui", |v| v["renderer"] = serde_json::json!(backend))
}
/// Renderer selection lives in a text-only modal, like the model/session picker.
#[derive(Clone)]
pub struct Picker {
    pub cursor: usize,
}
impl Picker {
    pub fn new(active: Backend) -> Self {
        Self {
            cursor: usize::from(active != Backend::Cpu),
        }
    }
}
#[cfg(test)]
fn test_shape_texts(shapes: &[egui::epaint::ClippedShape]) -> Vec<String> {
    shapes
        .iter()
        .filter_map(|s| match &s.shape {
            egui::Shape::Text(t) => Some(t.galley.text().to_owned()),
            _ => None,
        })
        .collect()
}
pub fn trigger(ui: &mut egui::Ui, active: Backend) -> bool {
    let response = ui.add(
        egui::Label::new(
            egui::RichText::new(active.status())
                .size(12.0)
                .color(crate::theme::TEAL),
        )
        .sense(egui::Sense::click()),
    );
    preview_bounds("selector", response.rect);
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text("Choose renderer · CPU uses less memory; GPU uses native graphics acceleration. A change restarts Feather.")
        .clicked()
}
pub fn picker_window(
    ctx: &egui::Context,
    picker: &mut Picker,
    active: Backend,
    enabled: bool,
    open: &mut bool,
) -> Option<Backend> {
    let options = [Backend::Cpu, Backend::accelerated()];
    let (up, down, enter) = ctx.input_mut(|i| {
        (
            i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp),
            i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown),
            i.consume_key(egui::Modifiers::NONE, egui::Key::Enter),
        )
    });
    if enabled {
        if down {
            picker.cursor = (picker.cursor + 1) % options.len();
        }
        if up {
            picker.cursor = (picker.cursor + options.len() - 1) % options.len();
        }
    }
    let highlight = picker.cursor;
    let mut picked = None;
    let mut hover = None;

    sheet(ctx, "renderer-picker", "Choose renderer")
        .width(360.0)
        .scrim(Scrim::Soft)
        .lead("A change restarts Feather; your session and draft are kept.")
        .body(|ui| {
            ui.add_enabled_ui(enabled, |ui| {
                for (index, backend) in options.into_iter().enumerate() {
                    let current = backend == active;
                    let text = match backend {
                        Backend::Cpu => "CPU · lower memory",
                        _ => "GPU · native graphics acceleration",
                    };
                    let response = choice_row(
                        ui,
                        text,
                        if current { "current" } else { "" },
                        index == highlight,
                        current,
                    );
                    preview_bounds(backend.env(), response.rect);
                    if response.hovered()
                        && ui.input(|i| {
                            i.events
                                .iter()
                                .any(|e| matches!(e, egui::Event::PointerMoved(_)))
                        })
                    {
                        hover = Some(index);
                    }
                    if response.clicked() {
                        picked = Some(backend);
                    }
                    ui.add_space(4.0);
                }
            });
            if !enabled {
                ui.add_space(8.0);
                hint(
                    ui,
                    "Finish or stop the response and pending sign-in before changing renderer.",
                );
            }
        })
        .bottom(|ui| {
            footnote(ui, "↑ / ↓ choose · Enter selects · Esc closes");
        })
        .show(open);

    if let Some(index) = hover {
        picker.cursor = index;
    }
    if picked.is_none() && enabled && enter {
        picked = Some(options[highlight]);
    }
    picked
}
fn preview_bounds(name: &str, rect: egui::Rect) {
    if std::env::var("FEATHER_UI_PREVIEW").ok().as_deref() != Some("1") {
        return;
    }
    if let Ok(dir) = std::env::var("FEATHER_UI_PREVIEW_RENDERER_HITS") {
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(
            Path::new(&dir).join(format!("{name}.txt")),
            format!("{} {}", rect.center().x, rect.center().y),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn legacy_wgl_selection_cannot_reenter_windows_gl() {
        if cfg!(windows) {
            assert_eq!(Backend::OpenGl.safe_for_platform(), Backend::Wgpu);
            assert_eq!(Backend::accelerated(), Backend::Wgpu);
        }
        assert_eq!(Backend::Cpu.safe_for_platform(), Backend::Cpu);
    }
    #[test]
    fn cpu_default_and_renderer_only_patch_preserve_other_config() {
        let home = std::env::temp_dir().join(feather_core::provider::gen_id("renderer-test"));
        assert_eq!(load(&home), Backend::Cpu);
        feather::config_store::write(&home, "gui", &serde_json::json!({"other":17})).unwrap();
        feather::config_store::write(
            &home,
            "settings",
            &serde_json::json!({"model":"synthetic/model","thinking":"high"}),
        )
        .unwrap();
        save(&home, Backend::OpenGl).unwrap();
        assert_eq!(load(&home), Backend::OpenGl);
        assert_eq!(
            feather::config_store::read(&home, "gui").unwrap().unwrap()["other"],
            17
        );
        assert_eq!(
            feather::config_store::read(&home, "settings")
                .unwrap()
                .unwrap()["model"],
            "synthetic/model"
        );
        std::fs::remove_dir_all(home).unwrap();
    }
    #[test]
    fn display_labels_are_explicit() {
        assert_eq!(Backend::Cpu.status(), "Render: CPU");
        assert_eq!(Backend::OpenGl.status(), "Render: GPU");
        assert_eq!(Backend::from_env("OpenGL"), Some(Backend::OpenGl));
    }
    #[test]
    fn picker_keyboard_selects_or_disables_backend_without_a_persistent_combo() {
        let ctx = egui::Context::default();
        crate::theme::apply_theme(&ctx);
        let mut picker = Picker::new(Backend::Cpu);
        let mut open = true;
        for _ in 0..3 {
            let _ = ctx.run(Default::default(), |ctx| {
                assert_eq!(
                    picker_window(ctx, &mut picker, Backend::Cpu, true, &mut open),
                    None
                );
            });
        }
        let key = |key| egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        };
        let mut picked = None;
        let out = ctx.run(
            egui::RawInput {
                events: vec![key(egui::Key::ArrowDown), key(egui::Key::Enter)],
                ..Default::default()
            },
            |ctx| {
                picked = picker_window(ctx, &mut picker, Backend::Cpu, true, &mut open);
            },
        );
        assert_eq!(picked, Some(Backend::accelerated()));
        assert!(test_shape_texts(&out.shapes)
            .iter()
            .any(|s| s == "Choose renderer"));
        let _ = ctx.run(
            egui::RawInput {
                events: vec![key(egui::Key::Enter)],
                ..Default::default()
            },
            |ctx| {
                picked = picker_window(ctx, &mut picker, Backend::Cpu, false, &mut open);
            },
        );
        assert_eq!(picked, None);
    }
}
