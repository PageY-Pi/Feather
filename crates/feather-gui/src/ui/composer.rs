//! The composer: suggestion menu, text box, footer hints and status bar.
//!
//! Owns the region below the transcript. `status_bar` lives here because it is
//! the composer's bottom row, not a separate panel.

use crate::theme::{self, soft_shadow, INK, MUTED, PANEL_2, RED, SOFT_ACCENT, TEAL, TEXT};
use crate::{
    commands, renderer_choice, rewind_choices, skill_choices, suggestion_cache, FeatherApp,
};
use eframe::egui;
impl FeatherApp {
    pub(crate) fn input_bar(&mut self, ui: &mut egui::Ui) {
        // Intentional conversation/composer divider. The opaque GPU canvas
        // eliminates the accidental antialiased panel-edge seam separately.
        let boundary = ui.available_rect_before_wrap();
        let y = ui.painter().round_to_pixel_center(boundary.top());
        ui.painter().line_segment(
            [
                egui::pos2(boundary.left(), y),
                egui::pos2(boundary.right(), y),
            ],
            egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(209, 205, 196)),
        );
        ui.add_space(8.0);
        let width = (theme::content_width(ui.available_width()) + 30.0).min(ui.available_width());
        let margin = ((ui.available_width() - width) * 0.5).max(0.0);
        ui.horizontal_top(|ui| {
            ui.add_space(margin);
            ui.vertical(|ui| {
                ui.set_width(width);
                let options = self.cached_suggestions();
                if !options.is_empty()
                    && !self.show_login
                    && !self.show_models
                    && self.picker.is_none()
                {
                    let mut selected = None;
                    let mut cursor = self.command_cursor;
                    egui::Frame::none()
                        .fill(SOFT_ACCENT)
                        .rounding(12.0)
                        .inner_margin(10.0)
                        .show(ui, |ui| {
                            ui.small(if skill_choices::query(&self.input).is_some() {
                                "Skills · ↑ / ↓ choose · Tab / Enter insert"
                            } else if rewind_choices::query(&self.input).is_some() {
                                "Turns · ↑ / ↓ choose · Enter rewinds"
                            } else {
                                "Commands · ↑ / ↓ choose · Tab / Enter select"
                            });
                            egui::ScrollArea::vertical()
                                .id_salt("slash-commands")
                                .max_height(230.0)
                                .show(ui, |ui| {
                                    for (index, c) in options.iter().enumerate() {
                                        let selected_row = index == cursor;
                                        let response = egui::Frame::none()
                                            .fill(if selected_row {
                                                theme::USER_BUBBLE
                                            } else {
                                                PANEL_2
                                            })
                                            .rounding(10.0)
                                            .inner_margin(8.0)
                                            .show(ui, |ui| {
                                                ui.set_width(ui.available_width());
                                                ui.horizontal(|ui| {
                                                    ui.label(egui::RichText::new(&c.0).color(TEAL));
                                                    ui.with_layout(
                                                        egui::Layout::right_to_left(
                                                            egui::Align::Center,
                                                        ),
                                                        |ui| {
                                                            ui.add(
                                                                egui::Label::new(
                                                                    egui::RichText::new(&c.1)
                                                                        .small(),
                                                                )
                                                                .truncate(),
                                                            );
                                                        },
                                                    );
                                                });
                                            })
                                            .response;
                                        let hit = ui.interact(
                                            response.rect,
                                            ui.id().with(index),
                                            egui::Sense::click(),
                                        );
                                        if hit.hovered() {
                                            if ui.input(|i| {
                                                i.events.iter().any(|e| {
                                                    matches!(e, egui::Event::PointerMoved(_))
                                                })
                                            }) {
                                                cursor = index;
                                            }
                                            ui.painter().rect_stroke(
                                                response.rect,
                                                10.0,
                                                egui::Stroke::new(1.5_f32, TEAL),
                                            );
                                        }
                                        if hit.clicked() {
                                            selected = Some(c.clone());
                                        }
                                        if selected_row
                                            && ui.input(|i| {
                                                i.key_pressed(egui::Key::ArrowUp)
                                                    || i.key_pressed(egui::Key::ArrowDown)
                                            })
                                        {
                                            response.scroll_to_me(Some(egui::Align::Center));
                                        }
                                    }
                                });
                        });
                    drop(options);
                    self.command_cursor = cursor;
                    if let Some(c) = selected {
                        self.accept_suggestion(c, ui.ctx());
                    }
                } else if options.is_empty()
                    && !self.show_login
                    && !self.show_models
                    && self.picker.is_none()
                {
                    drop(options);
                    if let Some(status) = self.suggestion_status() {
                        egui::Frame::none()
                            .fill(SOFT_ACCENT)
                            .rounding(12.0)
                            .inner_margin(10.0)
                            .show(ui, |ui| {
                                ui.label(egui::RichText::new(status).small().color(MUTED));
                                if skill_choices::query(&self.input).is_some() {
                                    for diagnostic in
                                        self.skill_choices.resources.diagnostics.iter().take(2)
                                    {
                                        ui.label(
                                            egui::RichText::new(&diagnostic.message)
                                                .small()
                                                .color(RED),
                                        );
                                    }
                                }
                            });
                    }
                } else {
                    drop(options);
                }
                egui::Frame::none()
                    .fill(PANEL_2)
                    .rounding(22.0)
                    .shadow(soft_shadow())
                    .inner_margin(egui::Margin::symmetric(19.0, 15.0))
                    .show(ui, |ui| {
                        let id = egui::Id::new("feather-composer");
                        // Submit keys are consumed by the named-action registry
                        // before TextEdit sees them; overrides remain authoritative.
                        // This scroll area owns caret scroll requests; they must not
                        // leak into the transcript rendered after the bottom panel.
                        let response = egui::ScrollArea::vertical()
                            .id_salt("composer-scroll")
                            .max_height(84.0)
                            .min_scrolled_height(48.0)
                            .auto_shrink([false, true])
                            .animated(false)
                            .show(ui, |ui| {
                                ui.add(
                                    egui::TextEdit::multiline(&mut self.input)
                                        .id(id)
                                        .font(egui::FontId::proportional(15.5))
                                        .text_color(TEXT)
                                        .hint_text(
                                            egui::RichText::new("What's on your mind?")
                                                .color(MUTED),
                                        )
                                        .desired_rows(2)
                                        .desired_width(ui.available_width())
                                        .frame(false),
                                )
                            })
                            .inner;
                        if !self.modal_open() && !ui.ctx().wants_keyboard_input() {
                            response.request_focus();
                        }
                        ui.add_space(5.0);
                        ui.horizontal(|ui| {
                            let hint = if self.queued_hint > 0 {
                                format!(
                                    "{} queued follow-up(s) · Enter queues · Esc stops",
                                    self.queued_hint
                                )
                            } else if self.running {
                                "Enter queues a follow-up · Esc stops".into()
                            } else {
                                "Enter to send · Shift+Enter for a new line · @ to reference a file"
                                    .into()
                            };
                            let (hint_rect, _) = ui.allocate_exact_size(
                                egui::vec2(ui.available_width().max(40.0), 22.0),
                                egui::Sense::hover(),
                            );
                            let galley = ui.fonts(|f| {
                                let mut job = egui::text::LayoutJob::simple(
                                    hint,
                                    egui::FontId::proportional(11.5),
                                    MUTED,
                                    hint_rect.width(),
                                );
                                job.wrap.max_rows = 1;
                                job.wrap.break_anywhere = true;
                                f.layout_job(job)
                            });
                            ui.painter().galley(
                                egui::pos2(
                                    hint_rect.left(),
                                    hint_rect.center().y - galley.size().y * 0.5,
                                ),
                                galley,
                                MUTED,
                            );
                        });
                    });
                ui.add_space(10.0);
                self.status_bar(ui);
                ui.add_space(16.0);
            });
        });
    }

    pub(crate) fn suggestion_status(&self) -> Option<String> {
        if skill_choices::query(&self.input).is_some() {
            return Some(if !self.skill_choices.loaded {
                "Loading skills…".into()
            } else if self.skill_choices.resources.skills.is_empty() {
                format!(
                    "No usable skills found in {} or {}.",
                    self.home.join("skills").display(),
                    self.root.join(".feather/skills").display()
                )
            } else {
                "No skills match your search.".into()
            });
        }
        if rewind_choices::query(&self.input).is_some() {
            return Some(if let Some(error) = &self.rewind_turns.error {
                format!("Cannot load turns: {error}")
            } else if !self.rewind_turns.loaded {
                "Loading committed turns…".into()
            } else if self.agent.is_none() && !self.preview {
                "No active session. Resume a saved session to choose a turn.".into()
            } else if self.rewind_turns.rows.is_empty() {
                "No committed turns yet. Turns appear after a request is saved.".into()
            } else {
                "No turns match your search.".into()
            });
        }
        None
    }
    pub(crate) fn accept_suggestion(
        &mut self,
        choice: (String, String, bool),
        ctx: &egui::Context,
    ) {
        self.input = choice.0;
        self.command_cursor = 0;
        if choice.2 {
            self.submit();
        }
        let id = egui::Id::new("feather-composer");
        let mut state = egui::TextEdit::load_state(ctx, id).unwrap_or_default();
        state
            .cursor
            .set_char_range(Some(egui::text::CCursorRange::one(
                egui::text::CCursor::new(self.input.chars().count()),
            )));
        state.store(ctx, id);
        ctx.memory_mut(|m| m.request_focus(id));
    }

    pub(crate) fn cached_suggestions(&self) -> std::cell::Ref<'_, [suggestion_cache::Suggestion]> {
        let menu_open = skill_choices::query(&self.input).is_some()
            || rewind_choices::query(&self.input).is_some()
            || (self.input.starts_with('/') && !self.input.chars().any(char::is_whitespace));
        let cache_key = if menu_open { self.input.as_str() } else { "" };
        self.suggestions.get(
            cache_key,
            self.skill_choices.revision,
            self.rewind_turns.revision,
            || self.input_suggestions(),
        )
    }
    pub(crate) fn input_suggestions(&self) -> Vec<(String, String, bool)> {
        if skill_choices::query(&self.input).is_some() {
            return self.skill_choices.suggestions(&self.input);
        }
        if rewind_choices::query(&self.input).is_some() {
            return self
                .rewind_turns
                .suggestions(&self.input)
                .into_iter()
                .map(|(a, b)| (a, b, true))
                .collect();
        }
        commands::suggestions(&self.input)
            .into_iter()
            .map(|c| {
                (
                    format!("/{}{}", c.name, if c.args { " " } else { "" }),
                    c.description.into(),
                    !c.args,
                )
            })
            .collect()
    }
    pub(crate) fn measure_status(ui: &egui::Ui, text: &str) -> f32 {
        ui.fonts(|fonts| {
            fonts.layout_no_wrap(
                text.to_owned(),
                egui::FontId::proportional(12.0),
                egui::Color32::WHITE,
            )
        })
        .size()
        .x
        .ceil()
    }

    pub(crate) fn status_bar(&mut self, ui: &mut egui::Ui) {
        let available = ui.available_width();
        let tight = available < 620.0;
        // Uniform rhythm: every item occupies only its measured (capped) width
        // and all items share one 7pt gap, so "a · b" is spaced identically on
        // both halves of the row. Fixed slots previously left dead air between
        // short values and their separator.
        const GAP: f32 = 7.0;
        let dot = Self::measure_status(ui, "·");
        let workspace = self
            .root
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let short_id = self.session_footer_label();
        let session_full = format!("{workspace} · {short_id}");
        let session_text = if tight {
            short_id.as_str()
        } else {
            session_full.as_str()
        };
        let model_full = self.settings.model.clone();
        let model_text = model_full
            .split_once('/')
            .map_or(model_full.as_str(), |(_, id)| id);
        let level = self.current_thinking();
        let effort_text = if tight {
            format!("effort {level}")
        } else {
            format!("thinking {level}")
        };
        let throughput = {
            let label = self.metrics.label(self.now());
            if label.is_empty() {
                "—".to_string()
            } else {
                label
            }
        };
        let tps_text = format!("TPS {throughput}");
        let proxy_text = self.proxies.label();
        let limit_text = self.limits.label();
        // Widths may only be reduced for overflow; never expanded beyond the
        // measured text, otherwise short values regain the old dead-air gaps.
        let session_natural = Self::measure_status(ui, session_text);
        let model_natural = Self::measure_status(ui, model_text);
        let mut session_w = session_natural.min(if tight { 150.0 } else { 300.0 });
        let mut model_w = model_natural.min(if tight { 150.0 } else { 220.0 });
        let effort_w = Self::measure_status(ui, &effort_text).min(128.0);
        // Telemetry is the only purely informational footer item, so at narrow
        // widths it yields its space to keep every control visible and
        // un-truncated (truncation also breaks the uniform separator rhythm).
        let show_tps = !tight;
        let tps_w = if show_tps {
            Self::measure_status(ui, &tps_text).min(120.0)
        } else {
            0.0
        };
        let render_w = Self::measure_status(ui, self.renderer.status());
        let proxy_w = Self::measure_status(ui, &proxy_text).min(190.0);
        let limit_w = Self::measure_status(ui, &limit_text).min(150.0);
        let right_width = proxy_w + limit_w + render_w + dot * 2.0 + 3.0 * GAP;
        let gutter = 24.0;
        let left_width = (available - right_width - gutter).max(100.0);
        // The natural width follows what is actually rendered: without TPS
        // there is one fewer separator and two fewer gaps.
        let natural = if show_tps {
            session_w + model_w + effort_w + tps_w + 3.0 * dot + 6.0 * GAP
        } else {
            session_w + model_w + effort_w + 2.0 * dot + 4.0 * GAP
        };
        if natural > left_width {
            let identities = (session_w + model_w).max(1.0);
            let needed = (natural - left_width).min((identities - 40.0).max(0.0));
            let factor = (identities - needed) / identities;
            session_w = (session_w * factor).max(24.0).min(session_natural);
            model_w = (model_w * factor).max(24.0).min(model_natural);
        }
        ui.horizontal(|ui| {
            let rect=ui.available_rect_before_wrap();
            let y=ui.painter().round_to_pixel_center(rect.top()-3.0);
            ui.painter().line_segment([egui::pos2(rect.left(),y),egui::pos2(rect.right(),y)],egui::Stroke::new(1.0_f32,egui::Color32::from_rgb(229,224,216)));
            ui.set_min_height(28.0);ui.spacing_mut().item_spacing.x=0.0;
            ui.allocate_ui_with_layout(egui::vec2(left_width,28.0),egui::Layout::left_to_right(egui::Align::Center),|ui| {
                ui.set_min_width(left_width);
                ui.spacing_mut().item_spacing.x=GAP;
                if ui.add_sized([session_w,22.0],egui::Label::new(egui::RichText::new(session_text).size(12.0).color(TEAL)).halign(egui::Align::Min).truncate().sense(egui::Sense::click()))
                    .on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text(format!("{session_full}\nWorkspace sessions · Ctrl+Shift+R")).clicked(){self.open_session_picker();}
                ui.label(egui::RichText::new("·").size(12.0).color(MUTED));
                if ui.add_sized([model_w,22.0],egui::Label::new(egui::RichText::new(model_text).size(12.0).color(TEAL)).halign(egui::Align::Min).truncate().sense(egui::Sense::click()))
                    .on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text(format!("{}\nClick or Ctrl+L to choose a model",self.settings.model)).clicked(){self.toggle_models();}
                ui.label(egui::RichText::new("·").size(12.0).color(MUTED));
                if ui.add_sized([effort_w,22.0],egui::Label::new(egui::RichText::new(&effort_text).size(12.0).color(TEAL)).halign(egui::Align::Min).truncate().sense(egui::Sense::click()))
                    .on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text("Click to adjust effort · Ctrl+Shift+T · Shift+Tab cycles").clicked(){self.open_thinking_picker();}
                if show_tps {
                    ui.label(egui::RichText::new("·").size(12.0).color(MUTED));
                    ui.add_sized([tps_w,22.0],egui::Label::new(egui::RichText::new(&tps_text).size(12.0).color(INK)).halign(egui::Align::Min).truncate())
                        .on_hover_text("While streaming: ~ is an estimate. At completion: provider output tokens / generation time.");
                }
            });
            ui.add_space(gutter);
            ui.allocate_ui_with_layout(egui::vec2(right_width,28.0),egui::Layout::right_to_left(egui::Align::Center),|ui| {
                ui.spacing_mut().item_spacing.x=GAP;
                if ui.add_sized([proxy_w,22.0],egui::Label::new(egui::RichText::new(&proxy_text).size(12.0).color(TEAL)).halign(egui::Align::Min).truncate().sense(egui::Sense::click()))
                    .on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text("Choose or turn off proxy for Feather requests · applies to next request, no restart").clicked(){self.open_request_picker(false);}
                ui.label(egui::RichText::new("·").size(12.0).color(MUTED));
                let limit_hint = if self.limits.limit.bytes_per_second == 0 {
                    "Cap how much bandwidth Feather uses · currently unlimited".to_string()
                } else {
                    format!(
                        "Feather is limited to {}/s for model traffic\nClick to change or remove the limit",
                        feather::netlimit::human(self.limits.limit.bytes_per_second)
                    )
                };
                if ui.add_sized([limit_w,22.0],egui::Label::new(egui::RichText::new(&limit_text).size(12.0).color(if self.limits.limit.bytes_per_second==0{MUTED}else{TEAL})).halign(egui::Align::Min).truncate().sense(egui::Sense::click()))
                    .on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text(limit_hint).clicked(){self.open_rate_limit_picker();}
                ui.label(egui::RichText::new("·").size(12.0).color(MUTED));
                if renderer_choice::trigger(ui,self.renderer){self.open_renderer_picker();}
            });
        });
    }
}
