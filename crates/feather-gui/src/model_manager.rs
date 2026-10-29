//! Custom compatible API/model editor. Persistent mutations happen in main;
//! this renderer emits intents so failures remain visible and retryable.
use eframe::egui;
use feather::models::{self, ModelEntry, ProviderEntry};
use serde_json::json;
#[derive(Default, Clone)]
pub struct ModelManager {
    pub enabled: bool,
    pub provider: String,
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub name: String,
    pub anthropic: bool,
    pub reasoning_not_supported: bool,
    pub tokens: String,
    pub error: Option<String>,
    pub confirm_delete: Option<(String, String)>,
    cached_models: Option<models::ModelsFile>,
    cached_home: Option<std::path::PathBuf>,
    loaded_at: Option<std::time::Instant>,
    /// Global context drafts, shared by every model. Seeded once from settings.
    pub context: String,
    pub reserve: String,
    pub preserved: String,
    context_seeded: bool,
}
pub enum Action {
    Save(String, ProviderEntry, ModelEntry),
    /// Global context/compaction budget for every model.
    SaveContext(u64, u64, u64),
    Delete(String, String),
}
impl ModelManager {
    pub fn invalidate_catalog(&mut self) {
        self.loaded_at = None;
    }
    fn refresh_catalog(&mut self, home: &std::path::Path) {
        if self.cached_home.as_deref() != Some(home)
            || self
                .loaded_at
                .map_or(true, |at| at.elapsed() > std::time::Duration::from_secs(5))
        {
            self.cached_models = models::saved_for(home).ok();
            self.cached_home = Some(home.to_path_buf());
            self.loaded_at = Some(std::time::Instant::now());
        }
    }
    /// Global context budget: one row of settings drives every model. The agent
    /// runs with `min(global context, model window)`, so selecting a smaller model
    /// never sends more tokens than that model accepts.
    pub fn show_context_settings(
        &mut self,
        ui: &mut egui::Ui,
        catalog: Option<&models::ModelsFile>,
        active: &str,
        global: (u64, u64, u64),
        running: bool,
    ) -> Option<Action> {
        if !self.context_seeded {
            self.context_seeded = true;
            self.context = global.0.to_string();
            self.reserve = global.1.to_string();
            self.preserved = global.2.to_string();
        }
        let mut action = None;
        egui::CollapsingHeader::new(
            egui::RichText::new("Context settings (all models)")
                .size(13.5)
                .color(crate::theme::TEAL),
        )
        .id_salt("global-context-settings")
        .default_open(false)
        .show(ui, |ui| {
            egui::Grid::new("global-context-settings-grid")
                .num_columns(2)
                .spacing(egui::vec2(18.0, 8.0))
                .show(ui, |ui| {
                    ui.label("Context tokens");
                    ui.text_edit_singleline(&mut self.context);
                    ui.end_row();
                    ui.label("Compaction reserve tokens");
                    ui.text_edit_singleline(&mut self.reserve);
                    ui.end_row();
                    ui.label("Preserved recent tokens");
                    ui.text_edit_singleline(&mut self.preserved);
                    ui.end_row();
                });

            let parse = |value: &str| value.trim().parse::<u64>().ok().filter(|n| *n > 0);
            let parsed = parse(&self.context)
                .zip(parse(&self.reserve))
                .zip(parse(&self.preserved));
            match parsed {
                Some(((context, reserve), _)) => {
                    let model = catalog
                        .and_then(|cat| models::find(cat, active))
                        .map(|(_, _, m)| m);
                    let cap = model.and_then(|m| m.context_window);
                    let effective = models::effective_context_window(context, model);
                    match cap {
                        Some(cap) if cap > effective => ui.small(format!(
                            "{active} accepts {cap} tokens: running with {effective} (context − reserve = {} triggers compaction).",
                            effective.saturating_sub(reserve)
                        )),
                        _ => ui.small(format!(
                            "Running with {effective} tokens (context − reserve = {} triggers compaction). Max output is not reserved.",
                            effective.saturating_sub(reserve)
                        )),
                    };
                }
                None => {
                    ui.small("Enter positive integer token limits.");
                }
            }
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(!running, egui::Button::new("Save context"))
                    .clicked()
                {
                    match parsed {
                        Some(((context, reserve), preserved)) => {
                            match models::validate_context_limits(context, reserve, preserved) {
                                Ok(()) => {
                                    self.error = None;
                                    action = Some(Action::SaveContext(context, reserve, preserved));
                                }
                                Err(error) => self.error = Some(error.to_string()),
                            }
                        }
                        None => {
                            self.error = Some("Token limits must be positive integers.".into());
                        }
                    }
                }
                if running {
                    ui.small("Stop the current response before changing limits.");
                }
            });
        });
        action
    }
    fn draft(&self) -> Result<Action, String> {
        let pid = self.provider.trim();
        if pid.is_empty()
            || pid.contains('/')
            || !pid
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
        {
            return Err(
                "Provider ID must contain letters, digits, - or _, without slashes.".into(),
            );
        }
        let model = self.model.trim();
        if model.is_empty() {
            return Err("Model ID is required (use the exact server model ID).".into());
        }
        let url = reqwest::Url::parse(self.base_url.trim())
            .map_err(|_| "Enter a valid base URL including /v1 if needed.".to_string())?;
        if !matches!(url.scheme(), "http" | "https")
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(
                "Use an HTTP(S) base URL without embedded credentials, query or fragment.".into(),
            );
        }
        let tokens = self
            .tokens
            .trim()
            .parse::<u64>()
            .ok()
            .filter(|n| *n > 0)
            .unwrap_or(8192);
        if models::is_managed_provider(pid) {
            return Err(format!(
                "{pid} is a built-in provider; its models are read-only. Pick another provider id."
            ));
        }
        let api = if self.anthropic {
            "anthropic-messages"
        } else {
            "openai-completions"
        };
        let p = serde_json::from_value(json!({"baseUrl":self.base_url.trim().trim_end_matches('/'),"api":api,"apiKey":self.api_key.trim(),"models":[]})).map_err(|e|e.to_string())?;
        let m = serde_json::from_value(json!({"id":model,"name":if self.name.trim().is_empty(){model}else{self.name.trim()},"reasoningNotSupported":self.reasoning_not_supported,"maxTokens":tokens,"input":["text"]})).map_err(|e|e.to_string())?;
        Ok(Action::Save(pid.into(), p, m))
    }
    /// The scrolling half of the form. The card owns the scroll area and the
    /// pinned footer, so this draws fields only — no nested scroll, so nothing
    /// can be clipped twice.
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        home: &std::path::Path,
        active: &str,
        running: bool,
    ) -> Option<Action> {
        let mut action = None;
        {
                ui.spacing_mut().text_edit_width = (ui.available_width() - 170.0).max(240.0);
                ui.label(
                    egui::RichText::new("Provider connection")
                        .size(16.0)
                        .strong()
                        .color(crate::theme::INK),
                );
                ui.label(
                    egui::RichText::new(
                        "Use the provider’s base URL, including /v1 when required.",
                    )
                    .size(12.0)
                    .color(crate::theme::MUTED),
                );
                ui.add_space(5.0);
                ui.horizontal_wrapped(|ui| {
                    ui.radio_value(&mut self.anthropic, false, "OpenAI compatible");
                    ui.add_space(6.0);
                    ui.radio_value(&mut self.anthropic, true, "Anthropic compatible");
                });
                egui::Grid::new("custom-api-form")
                    .striped(false)
                    .num_columns(2)
                    .spacing(egui::vec2(18.0, 8.0))
                    .min_col_width(120.0)
                    .show(ui, |ui| {
                        ui.label("Provider ID");
                        ui.text_edit_singleline(&mut self.provider);
                        ui.end_row();
                        ui.label("Base URL");
                        ui.text_edit_singleline(&mut self.base_url);
                        ui.end_row();
                        ui.label("API key or $ENV");
                        ui.add(egui::TextEdit::singleline(&mut self.api_key).password(true));
                        ui.end_row();
                        ui.label("Model ID");
                        ui.text_edit_singleline(&mut self.model);
                        ui.end_row();
                    });
                egui::CollapsingHeader::new(
                    egui::RichText::new("Advanced model settings")
                        .size(12.5)
                        .color(crate::theme::TEAL),
                )
                .id_salt("advanced-model-settings")
                .default_open(ui.ctx().screen_rect().height() > 700.0)
                .show(ui, |ui| {
                    egui::Grid::new("advanced-api-form")
                        .striped(false)
                        .num_columns(2)
                        .spacing(egui::vec2(18.0, 8.0))
                        .min_col_width(120.0)
                        .show(ui, |ui| {
                            ui.label("Display name");
                            ui.text_edit_singleline(&mut self.name);
                            ui.end_row();
                            ui.label("Max output tokens");
                            ui.text_edit_singleline(&mut self.tokens);
                            ui.end_row();
                            ui.label("Thinking");
                            ui.checkbox(
                                &mut self.reasoning_not_supported,
                                "Reasoning not supported",
                            );
                            ui.end_row();
                        });
                });
                ui.label(
                    egui::RichText::new("Effort: off · low · med · high · xhigh · max")
                        .size(12.0)
                        .color(crate::theme::MUTED),
                );
                ui.add_space(4.0);
                ui.separator();
                ui.label(
                    egui::RichText::new("Configured models")
                        .size(14.0)
                        .strong()
                        .color(crate::theme::INK),
                );
                // Show user-configured entries rather than thousands of bundled defaults.
                self.refresh_catalog(home);
                let custom = &self.cached_models;
                if !custom
                    .as_ref()
                    .is_some_and(|c| c.providers.values().any(|p| !p.models.is_empty()))
                {
                    ui.label(
                        egui::RichText::new("No custom models yet. Add one above to get started.")
                            .size(12.0)
                            .color(crate::theme::MUTED),
                    );
                }
                egui::ScrollArea::vertical()
                    .id_salt("managed-models")
                    .max_height(240.0)
                    .show(ui, |ui| {
                        if let Some(cat) = &custom {
                            for (pid, p) in &cat.providers {
                                // Built-in/OAuth providers own their model list; only
                                // credentials are user-managed, so nothing here is editable.
                                if models::is_managed_provider(pid) {
                                    continue;
                                }
                                for m in &p.models {
                                    let current = format!("{pid}/{}", m.id) == active;
                                    ui.horizontal(|ui| {
                                        ui.label(format!(
                                            "{pid}/{}{}",
                                            m.id,
                                            if current { " · selected" } else { "" }
                                        ));
                                        if ui
                                            .add_enabled(!running, egui::Button::new("Edit"))
                                            .clicked()
                                        {
                                            self.provider = pid.clone();
                                            self.base_url = p.base_url.clone().unwrap_or_default();
                                            self.api_key = p.api_key.clone().unwrap_or_default();
                                            self.model = m.id.clone();
                                            self.name = m.name.clone().unwrap_or_default();
                                            self.anthropic =
                                                p.api.as_deref() == Some("anthropic-messages");
                                            self.reasoning_not_supported =
                                                m.reasoning_not_supported;
                                            self.tokens = m
                                                .max_tokens
                                                .map(|v| v.to_string())
                                                .unwrap_or_default();
                                        }
                                        if ui
                                            .add_enabled(!running, egui::Button::new("Delete"))
                                            .clicked()
                                        {
                                            self.confirm_delete = Some((pid.clone(), m.id.clone()));
                                        }
                                    });
                                    ui.small(if m.reasoning_not_supported {
                                        "Reasoning not supported"
                                    } else {
                                        "off · low · med · high · xhigh · max"
                                    });
                                }
                            }
                            if custom
                                .as_ref()
                                .is_some_and(|c| c.providers.keys().any(|pid| models::is_managed_provider(pid)))
                            {
                                ui.small(
                                    "Built-in providers (OAuth sign-in and bundled API-key plans) keep their own model lists — connect them in 'OAuth / built-in providers'.",
                                );
                            }
                        }
                    });
                if let Some((pid, id)) = self.confirm_delete.clone() {
                    ui.separator();
                    ui.colored_label(
                        crate::theme::RED,
                        format!("Delete model {pid}/{id}? This does not delete sessions."),
                    );
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(!running, egui::Button::new("Confirm delete"))
                            .clicked()
                        {
                            action = Some(Action::Delete(pid, id));
                            self.confirm_delete = None;
                        }
                        if ui.button("Cancel").clicked() {
                            self.confirm_delete = None;
                        }
                    });
                }
                ui.add_space(16.0);
        }
        action
    }

    /// The pinned half: the validation line, then the two buttons. Kept apart
    /// from `show` so the card can place it outside the scroll region.
    pub fn show_footer(&mut self, ui: &mut egui::Ui, running: bool) -> Option<Action> {
        let mut action = None;
        if let Some(error) = &self.error {
            crate::dialog::error_line(ui, error);
            ui.add_space(10.0);
        }
        ui.horizontal(|ui| {
            let save = crate::dialog::button(ui, "Save model", crate::dialog::ButtonKind::Primary);
            if !running && save.clicked() {
                match self.draft() {
                    Ok(a) => {
                        self.error = None;
                        action = Some(a);
                    }
                    Err(e) => self.error = Some(e),
                }
            }
            if crate::dialog::button(ui, "Clear form", crate::dialog::ButtonKind::Ghost).clicked() {
                let enabled = self.enabled;
                *self = Self::default();
                self.enabled = enabled;
            }
        });
        action
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn compatible_form_validation() {
        let mut form = ModelManager::default();
        assert!(form.draft().is_err());
        form.provider = "my-proxy".into();
        form.base_url = "https://example.test/v1".into();
        form.model = "vendor/model".into();
        assert!(matches!(form.draft(), Ok(Action::Save(_, _, _))));
        form.base_url = "file:///tmp/api".into();
        assert!(form.draft().is_err());
    }

    #[test]
    fn builtin_providers_cannot_be_saved_from_the_form() {
        // Saving into a bundled provider id would overwrite its wire API and
        // hide its bundled models, so the form refuses it up front.
        let mut form = ModelManager::default();
        form.provider = "volcengine-plan".into();
        form.base_url = "https://ark.cn-beijing.volces.com/api/coding/v3".into();
        form.model = "deepseek-v4.1-flash".into();
        let Err(err) = form.draft() else {
            panic!("built-in provider ids must be refused");
        };
        assert!(err.contains("read-only"), "{err}");
    }
}
