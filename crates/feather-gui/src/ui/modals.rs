//! Modal windows and the startup/resume chooser.
//!
//! Filled in by the split of `main.rs`; see `ui/mod.rs` for the rationale.

use crate::theme::{self, INK, MUTED, PANEL_2, RED, SOFT_ACCENT, TEAL, TEXT};
use crate::{
    config, format_expires, keybindings, models, oauth, sessions, ChatRow, FeatherApp, RowKind,
    UiEvent,
};
use eframe::egui;
use feather_core::agent::AgentEvent;

impl FeatherApp {
    /// Startup/resume chooser modal. Closing it (×, Esc) or picking "new"
    /// returns to a fresh memory welcome without restoring any transcript.
    pub(crate) fn resume_window(&mut self, ctx: &egui::Context) {
        if self.picker.is_none() {
            return;
        }
        let mut open = true;
        let pick = {
            let chooser = self.picker.as_mut().expect("picker");
            sessions::picker_window(ctx, chooser, &mut open)
        };
        if let Some(pick) = pick {
            self.choose_pick(pick);
            return;
        }
        if !open {
            self.picker = None;
        }
    }

    /// Compact footer identity: the chosen session id, or `new` before one is
    /// materialized.
    pub(crate) fn session_footer_label(&self) -> String {
        if self.picker.is_some() {
            return "choosing".into();
        }
        match self.session_dir.file_name().and_then(|name| name.to_str()) {
            Some(id) if !id.is_empty() => sessions::short_id(id),
            _ => "new".into(),
        }
    }

    pub(crate) fn models_window(&mut self, ctx: &egui::Context) {
        if !self.show_models {
            return;
        }
        let mut picked = None;
        let mut open = self.show_models;
        let available: std::collections::BTreeSet<_> = self
            .catalog
            .as_ref()
            .map(|cat| {
                cat.providers
                    .iter()
                    .filter(|(pid, p)| self.provider_available(pid, p))
                    .map(|(pid, _)| pid.clone())
                    .collect()
            })
            .unwrap_or_default();
        egui::Window::new("Choose a model")
            .open(&mut open)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .order(egui::Order::Foreground)
            .collapsible(false)
            .resizable(true)
            .default_width(520.0)
            .show(ctx, |ui| {
                ui.label(
                    egui::RichText::new(
                        "Type to search · ↑ / ↓ choose · Enter selects · Esc closes",
                    )
                    .size(12.5)
                    .color(MUTED),
                );
                ui.add_sized(
                    [ui.available_width(), 30.0],
                    egui::TextEdit::singleline(&mut self.model_filter)
                        .id(egui::Id::new("model-search"))
                        .hint_text("Search models or providers…")
                        .text_color(TEXT),
                );
                let query = self.model_filter.to_lowercase();
                let flat: Vec<_> = self
                    .catalog
                    .as_ref()
                    .map(|cat| {
                        cat.providers
                            .iter()
                            .filter(|(pid, _)| available.contains(*pid))
                            .flat_map(|(pid, p)| {
                                p.models.iter().map(move |m| (pid.clone(), m.id.clone()))
                            })
                            .filter(|(pid, id)| {
                                query.is_empty()
                                    || format!("{pid}/{id}").to_lowercase().contains(&query)
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                self.model_cursor = self.model_cursor.min(flat.len().saturating_sub(1));
                let down =
                    ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown));
                let up = ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp));
                if !flat.is_empty() {
                    if down {
                        self.model_cursor = (self.model_cursor + 1) % flat.len();
                    }
                    if up {
                        self.model_cursor = (self.model_cursor + flat.len() - 1) % flat.len();
                    }
                    if !self.running
                        && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter))
                    {
                        picked = Some(flat[self.model_cursor].clone());
                    }
                }
                let highlighted = flat.get(self.model_cursor).cloned();
                if flat.is_empty() {
                    ui.label(
                        egui::RichText::new(
                            "No usable models match. Use +add to add an API or sign in.",
                        )
                        .color(MUTED),
                    );
                }
                if self.running {
                    ui.label(
                        egui::RichText::new("Model changes are available after the current run.")
                            .color(MUTED),
                    );
                }
                ui.add_enabled_ui(!self.running, |ui| {
                    if let Some(cat) = &self.catalog {
                        egui::ScrollArea::vertical()
                            .max_height(410.0)
                            .show(ui, |ui| {
                                for (pid, p) in &cat.providers {
                                    if !available.contains(pid) {
                                        continue;
                                    }
                                    let matching: Vec<_> = p
                                        .models
                                        .iter()
                                        .filter(|m| {
                                            query.is_empty()
                                                || format!("{pid}/{}", m.id)
                                                    .to_lowercase()
                                                    .contains(&query)
                                        })
                                        .collect();
                                    if matching.is_empty() {
                                        continue;
                                    }
                                    ui.add_space(10.0);
                                    ui.label(
                                        egui::RichText::new(pid).size(12.5).strong().color(TEAL),
                                    );
                                    for m in matching {
                                        let full = format!("{pid}/{}", m.id);
                                        let selected =
                                            highlighted.as_ref().is_some_and(|(provider, id)| {
                                                provider == pid && id == &m.id
                                            });
                                        // Show what the agent will really use: the
                                        // global context capped by the model window.
                                        let effective = models::effective_context_window(
                                            self.settings.context_window,
                                            Some(m),
                                        );
                                        let context = match m.context_window {
                                            Some(cap) if cap > effective => format!(
                                                "{}k of {}k context",
                                                effective / 1000,
                                                cap / 1000
                                            ),
                                            _ => format!("{}k context", effective / 1000),
                                        };
                                        let response = egui::Frame::none()
                                            .fill(if selected { SOFT_ACCENT } else { PANEL_2 })
                                            .rounding(12.0)
                                            .inner_margin(egui::Margin::symmetric(12.0, 8.0))
                                            .show(ui, |ui| {
                                                ui.set_width(ui.available_width());
                                                ui.horizontal(|ui| {
                                                    ui.add(
                                                        egui::Label::new(
                                                            egui::RichText::new(&m.id)
                                                                .size(14.0)
                                                                .color(TEXT),
                                                        )
                                                        .truncate(),
                                                    );
                                                    ui.with_layout(
                                                        egui::Layout::right_to_left(
                                                            egui::Align::Center,
                                                        ),
                                                        |ui| {
                                                            ui.label(
                                                                egui::RichText::new(context)
                                                                    .size(11.5)
                                                                    .color(MUTED),
                                                            );
                                                        },
                                                    );
                                                });
                                            })
                                            .response;
                                        if selected && (down || up) {
                                            response.scroll_to_me(Some(egui::Align::Center));
                                        }
                                        let hit = ui.interact(
                                            response.rect,
                                            ui.id().with(full),
                                            egui::Sense::click(),
                                        );
                                        if hit.hovered() {
                                            if ui.input(|i| {
                                                i.events.iter().any(|e| {
                                                    matches!(e, egui::Event::PointerMoved(_))
                                                })
                                            }) {
                                                if let Some(index) = flat
                                                    .iter()
                                                    .position(|(p, id)| p == pid && id == &m.id)
                                                {
                                                    self.model_cursor = index;
                                                }
                                            }
                                            if !selected {
                                                ui.painter().rect_stroke(
                                                    response.rect,
                                                    12.0,
                                                    egui::Stroke::new(1.5_f32, TEAL),
                                                );
                                            }
                                        }
                                        if hit.clicked() {
                                            picked = Some((pid.clone(), m.id.clone()));
                                        }
                                    }
                                }
                            });
                    }
                });
            });
        self.show_models = open;
        if let Some((pid, id)) = picked {
            self.select_model(&pid, &id);
            self.show_models = false;
        }
    }

    /// Slider popup for thinking effort (ChatGPT-web style).
    pub(crate) fn thinking_picker_window(&mut self, ctx: &egui::Context) {
        if !self.show_thinking_picker {
            return;
        }
        let levels = self.effort_levels();
        let max = levels.len() - 1;
        self.thinking_slider = self.thinking_slider.min(max);
        let mut open = self.show_thinking_picker;
        let mut apply = None;
        egui::Window::new("Thinking effort")
            .open(&mut open)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .order(egui::Order::Foreground)
            .collapsible(false)
            .resizable(false)
            .default_width(440.0)
            .show(ctx, |ui| {
                ui.label(
                    egui::RichText::new("Give Feather a little more time to think.")
                        .size(13.5)
                        .color(MUTED),
                );
                ui.small("Manual effort presets · configure 'Reasoning not supported' in +add to disable.");
                ui.add_space(10.0);
                egui::Frame::none()
                    .fill(SOFT_ACCENT)
                    .rounding(16.0)
                    .inner_margin(egui::Margin::same(16.0))
                    .show(ui, |ui| {
                        ui.label(
                            egui::RichText::new(&levels[self.thinking_slider])
                                .size(21.0)
                                .strong()
                                .color(theme::TEAL_BRIGHT),
                        );
                        ui.add_space(6.0);
                        ui.spacing_mut().slider_width = (ui.available_width() - 12.0).max(120.0);
                        let response = ui.add_enabled(
                            max > 0,
                            egui::Slider::new(&mut self.thinking_slider, 0..=max)
                                .show_value(false)
                                .smart_aim(false),
                        );
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new(&levels[0]).size(12.0).color(MUTED));
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    ui.label(
                                        egui::RichText::new(&levels[max]).size(12.0).color(MUTED),
                                    );
                                },
                            );
                        });
                        if (response.changed() && !response.dragged()) || response.drag_stopped() {
                            apply = Some(levels[self.thinking_slider].clone());
                        }
                    });
                ui.add_space(10.0);
                ui.label(
                    egui::RichText::new(if max == 0 {
                        "This model has no adjustable thinking effort."
                    } else {
                        "Drag or use ← / → · release to apply · Esc or × to close"
                    })
                    .size(12.0)
                    .color(MUTED),
                );
            });
        self.show_thinking_picker = open;
        if let Some(level) = apply {
            if self.thinking_slider != self.thinking_applied {
                self.thinking_applied = self.thinking_slider;
                self.set_thinking(&level);
            }
        }
    }

    pub(crate) fn settings_window(&mut self, ctx: &egui::Context) {
        if !self.show_settings {
            return;
        }
        let mut reload = false;
        egui::Window::new(egui::RichText::new("Keybindings (pi parity)").color(TEXT))
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .order(egui::Order::Foreground)
            .collapsible(false)
            .resizable(true)
            .default_width(640.0)
            .show(ctx, |ui| {
                if ui.checkbox(&mut self.settings.auto_resume_latest, "Continue latest workspace session on startup").changed() {
                    if let Err(e)=feather::preferences::save_settings_patch(&config::feather_home().join("config.redb"),&serde_json::json!({"autoResumeLatest":self.settings.auto_resume_latest})) {
                        self.rows.push(ChatRow::new(RowKind::Error,e.to_string()));
                    }
                }
                ui.add_space(8.0);
                let path = config::feather_home().join("config.redb");
                ui.label(
                    egui::RichText::new(format!(
                        "Overrides: {}  — map an action to a key or a list of keys; an empty list disables it. Press F5 here to reload.",
                        path.display()
                    ))
                    .size(11.0)
                    .color(MUTED),
                );
                ui.add_space(4.0);
                reload = ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::F5));
                ui.label(egui::RichText::new("F1 shortcuts · Ctrl+L models · Ctrl+Shift+T thinking · Ctrl+Shift+N new session").size(12.5).color(INK));
                ui.add_space(6.0);
                egui::ScrollArea::vertical().max_height(460.0).show(ui, |ui| {
                    egui::Grid::new("kb_grid")
                        .num_columns(3)
                        .spacing([16.0, 3.0])
                        .show(ui, |ui| {
                            for (id, combos) in self.kb.table() {
                                ui.label(egui::RichText::new(id).size(11.0).color(TEXT));
                                let spec = if combos.is_empty() {
                                    "(disabled)".to_string()
                                } else {
                                    combos
                                        .iter()
                                        .map(|c| c.to_spec())
                                        .collect::<Vec<_>>()
                                        .join(", ")
                                };
                                ui.label(
                                    egui::RichText::new(spec)
                                        .size(11.0)
                                        .color(if self.kb.is_overridden(id) { TEAL } else { MUTED }),
                                );
                                if self.kb.is_overridden(id) {
                                    ui.label(
                                        egui::RichText::new("override")
                                            .size(10.0)
                                            .color(TEAL),
                                    );
                                } else {
                                    ui.label(egui::RichText::new("default").size(10.0).color(MUTED));
                                }
                                ui.end_row();
                            }
                        });
                });
            });
        if reload {
            self.kb = keybindings::Keybindings::load();
        }
    }

    pub(crate) fn spawn_login(&mut self, p: &'static oauth::OAuthProvider) {
        if self.preview {
            return;
        }
        if !self.auth_pending.insert(p.id) {
            return;
        }
        let tx = self.ev_tx.clone();
        let prompt_tx = tx.clone();
        let ctx = self.egui_ctx.clone();
        let provider = p.id;
        std::thread::spawn(move || {
            let notify = |pr: oauth::LoginPrompt| {
                let prompt = if pr.user_code.is_empty() {
                    format!("{provider}: continue sign-in at {}", pr.url)
                } else {
                    format!("{provider}: enter code {} at {}", pr.user_code, pr.url)
                };
                let _ = prompt_tx.send(UiEvent::Agent(AgentEvent::Notice(prompt)));
                ctx.request_repaint();
            };
            let result = oauth::run_login_with_notify(p, &notify)
                .and_then(|o| oauth::AuthStore::update(provider, Some(o.account)));
            let _ = tx.send(UiEvent::Auth {
                provider,
                action: "signed in",
                result,
            });
            ctx.request_repaint();
        });
    }

    pub(crate) fn spawn_auth_update(
        &mut self,
        provider: &'static str,
        account: Option<oauth::OAuthAccount>,
    ) {
        if self.preview {
            return;
        }
        if !self.auth_pending.insert(provider) {
            return;
        }
        let action = if account.is_some() {
            "API key saved"
        } else {
            "signed out (environment keys are unchanged)"
        };
        let tx = self.ev_tx.clone();
        let ctx = self.egui_ctx.clone();
        std::thread::spawn(move || {
            let result = oauth::AuthStore::update(provider, account);
            let _ = tx.send(UiEvent::Auth {
                provider,
                action,
                result,
            });
            ctx.request_repaint();
        });
    }

    /// `AuthStore` memoized on the config database's (mtime, len).
    ///
    /// The login window renders every frame, so reading the credential store
    /// from redb per frame is pure waste. The stamp keeps the behavior exact:
    /// any write — this process or another one — changes the file and forces a
    /// reload.
    pub(crate) fn cached_auth_store(home: &std::path::Path) -> Result<oauth::AuthStore, String> {
        fn stamp(path: &std::path::Path) -> Option<(u64, u64)> {
            let meta = std::fs::metadata(path).ok()?;
            let mtime = meta
                .modified()
                .ok()?
                .duration_since(std::time::UNIX_EPOCH)
                .ok()?
                .as_millis() as u64;
            Some((mtime, meta.len()))
        }
        type Cache = std::sync::Mutex<Option<(Option<(u64, u64)>, oauth::AuthStore)>>;
        static CACHE: std::sync::OnceLock<Cache> = std::sync::OnceLock::new();
        let path = home.join("config.redb");
        let current = stamp(&path);
        let cache = CACHE.get_or_init(|| std::sync::Mutex::new(None));
        if let Ok(guard) = cache.lock() {
            if let Some((cached, store)) = guard.as_ref() {
                if *cached == current {
                    return Ok(store.clone());
                }
            }
        }
        let store = oauth::AuthStore::load_from(&path)?;
        if let Ok(mut guard) = cache.lock() {
            *guard = Some((current, store.clone()));
        }
        Ok(store)
    }

    pub(crate) fn login_window(&mut self, ctx: &egui::Context) {
        if !self.show_login {
            return;
        }
        let auth_error = match Self::cached_auth_store(&self.home) {
            Ok(store) => {
                self.auth = store;
                None
            }
            Err(e) => Some(e),
        };
        let mut open = self.show_login;
        egui::Window::new("Add API / manage models")
            .open(&mut open)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .order(egui::Order::Foreground)
            .collapsible(false)
            .resizable(false)
            .default_width(if ctx.screen_rect().width() < 900.0 {
                500.0
            } else {
                540.0
            })
            .default_height((ctx.screen_rect().height() - 180.0).clamp(240.0, 650.0))
            .max_width((ctx.screen_rect().width() - 64.0).max(320.0))
            .max_height((ctx.screen_rect().height() - 100.0).max(220.0))
            .show(ctx, |ui| {
                ui.spacing_mut().icon_width = 22.0;
                ui.label(
                    egui::RichText::new(
                        "Connect a provider, then choose the models you want to use.",
                    )
                    .size(13.0)
                    .color(MUTED),
                );
                ui.add_space(6.0);
                if let Some(error) = &auth_error {
                    ui.colored_label(RED, error);
                }
                ui.horizontal(|ui| {
                    ui.selectable_value(
                        &mut self.model_manager.enabled,
                        true,
                        "Compatible API / models",
                    );
                    ui.selectable_value(
                        &mut self.model_manager.enabled,
                        false,
                        "OAuth / built-in providers",
                    );
                });
                if let Some(action) = self.model_manager.show_context_settings(
                    ui,
                    self.catalog.as_ref(),
                    &self.settings.model,
                    (
                        self.settings.context_window,
                        self.settings.compaction.reserve_tokens,
                        self.settings.compaction.keep_recent_tokens,
                    ),
                    self.running,
                ) {
                    self.manage_model(action);
                }
                ui.add_space(6.0);
                ui.add_space(6.0);
                if self.model_manager.enabled {
                    if ctx.screen_rect().height() < 700.0 {
                        ui.label(
                            egui::RichText::new("Scroll for more settings ↓ · Save stays below")
                                .size(12.0)
                                .color(TEAL),
                        );
                    }
                    let action = self.model_manager.show(
                        ui,
                        self.catalog.as_ref(),
                        &self.home,
                        &self.settings.model,
                        self.running,
                    );
                    if let Some(action) = action {
                        self.manage_model(action);
                    }
                    return;
                }
                ui.label(
                    egui::RichText::new(
                        "↑ / ↓ choose · Enter sign in · Delete sign out · Esc close",
                    )
                    .size(12.0)
                    .color(MUTED),
                );
                ui.label(
                    egui::RichText::new(
                        "OAuth opens your browser. Credentials renew automatically.",
                    )
                    .size(12.0)
                    .color(MUTED),
                );
                let key_id = egui::Id::new("login-api-key");
                let editing = ui.memory(|m| m.has_focus(key_id));
                let previous_selected = self.auth_selected;
                if !editing {
                    if ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown))
                    {
                        self.auth_selected = (self.auth_selected + 1) % oauth::PROVIDERS.len();
                    }
                    if ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp)) {
                        self.auth_selected = (self.auth_selected + oauth::PROVIDERS.len() - 1)
                            % oauth::PROVIDERS.len();
                    }
                }
                let selected = oauth::PROVIDERS[self.auth_selected];
                let pending = self.auth_pending.contains(selected.id);
                if !editing && !pending && auth_error.is_none() {
                    if ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Delete))
                        && self.auth.get(selected.id).is_some()
                    {
                        self.spawn_auth_update(selected.id, None);
                    }
                    if ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter)) {
                        if selected.kind == oauth::Flow::ApiKey {
                            ui.memory_mut(|m| m.request_focus(key_id));
                        } else {
                            self.spawn_login(selected);
                        }
                    }
                }
                ui.add_space(8.0);
                egui::ScrollArea::vertical()
                    .id_salt("auth-providers")
                    .max_height((ctx.screen_rect().height() - 260.0).clamp(150.0, 340.0))
                    .show(ui, |ui| {
                        for (index, p) in oauth::PROVIDERS.iter().enumerate() {
                            let selected = index == self.auth_selected;
                            let logged = self.auth.valid(p.id);
                            let pending = self.auth_pending.contains(p.id);
                            let status = self
                                .auth
                                .get(p.id)
                                .map(|a| {
                                    if a.kind == "api_key" {
                                        "API key saved".into()
                                    } else if logged {
                                        format_expires(a.expires)
                                    } else {
                                        "renew on next request".into()
                                    }
                                })
                                .unwrap_or_else(|| "not signed in".into());
                            let response = egui::Frame::none()
                                .fill(if selected { SOFT_ACCENT } else { PANEL_2 })
                                .rounding(12.0)
                                .inner_margin(egui::Margin::symmetric(12.0, 8.0))
                                .show(ui, |ui| {
                                    ui.set_width(ui.available_width());
                                    ui.horizontal(|ui| {
                                        let (rect, _) = ui.allocate_exact_size(
                                            egui::vec2(8.0, 8.0),
                                            egui::Sense::hover(),
                                        );
                                        ui.painter().circle_filled(
                                            rect.center(),
                                            3.0,
                                            if logged { TEAL } else { MUTED },
                                        );
                                        ui.label(egui::RichText::new(p.id).size(14.0).color(TEXT));
                                        ui.with_layout(
                                            egui::Layout::right_to_left(egui::Align::Center),
                                            |ui| {
                                                if pending {
                                                    ui.spinner();
                                                }
                                                ui.label(
                                                    egui::RichText::new(status)
                                                        .size(12.0)
                                                        .color(MUTED),
                                                );
                                            },
                                        );
                                    });
                                })
                                .response;
                            let hit = ui.interact(
                                response.rect,
                                ui.id().with(p.id),
                                egui::Sense::click(),
                            );
                            if hit.hovered() {
                                if ui.input(|i| {
                                    i.events
                                        .iter()
                                        .any(|e| matches!(e, egui::Event::PointerMoved(_)))
                                }) {
                                    self.auth_selected = index;
                                }
                                if !selected {
                                    ui.painter().rect_stroke(
                                        response.rect,
                                        12.0,
                                        egui::Stroke::new(1.5_f32, TEAL),
                                    );
                                }
                            }
                            if hit.clicked() {
                                self.auth_selected = index;
                            }
                            if selected && self.auth_selected != previous_selected {
                                response.scroll_to_me(Some(egui::Align::Center));
                            }
                        }
                    });
                let selected = oauth::PROVIDERS[self.auth_selected];
                if selected.kind == oauth::Flow::ApiKey {
                    ui.add_space(8.0);
                    ui.label(
                        egui::RichText::new(format!(
                            "{} · paste key, then Enter to save · Ctrl+O opens the key portal",
                            selected.id
                        ))
                        .size(12.0)
                        .color(MUTED),
                    );
                    if ui.input_mut(|i| i.consume_key(egui::Modifiers::CTRL, egui::Key::O)) {
                        if let Err(error) = oauth::open_browser(selected.authorize_url) {
                            self.rows.push(ChatRow::new(RowKind::Error, error));
                        }
                    }
                    let mut save = None;
                    ui.add_enabled_ui(
                        !self.auth_pending.contains(selected.id) && auth_error.is_none(),
                        |ui| {
                            let draft = self.api_key_drafts.entry(selected.id.into()).or_default();
                            let enter = ui.memory(|m| m.has_focus(key_id))
                                && ui.input_mut(|i| {
                                    i.consume_key(egui::Modifiers::NONE, egui::Key::Enter)
                                });
                            ui.add_sized(
                                [ui.available_width(), 30.0],
                                egui::TextEdit::singleline(draft)
                                    .id(key_id)
                                    .hint_text("Paste API key…")
                                    .password(true)
                                    .text_color(TEXT),
                            );
                            if enter && !draft.trim().is_empty() {
                                save = Some(draft.trim().to_string());
                                draft.clear();
                            }
                        },
                    );
                    if let Some(key) = save {
                        self.spawn_auth_update(
                            selected.id,
                            Some(oauth::OAuthAccount::api_key(key)),
                        );
                    }
                }
            });
        self.show_login = open;
    }
}
