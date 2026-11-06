//! The frame: per-frame entry points, panel layout and event draining.
//!
//! Filled in by the split of `main.rs`; see `ui/mod.rs` for the rationale.

use crate::theme::{self, INK, MUTED, PANEL, RED, SOFT_ACCENT};
use crate::{cards, keybindings, transcript, FeatherApp};
use eframe::egui;

impl FeatherApp {
    pub(crate) fn chat_panel(&mut self, ui: &mut egui::Ui) {
        if self.rows.is_empty() {
            let rect = ui.available_rect_before_wrap();
            ui.allocate_new_ui(
                egui::UiBuilder::new().max_rect(egui::Rect::from_center_size(
                    egui::pos2(rect.center().x, rect.top() + rect.height() * 0.43),
                    egui::vec2(rect.width().min(500.0), 270.0),
                )),
                |ui| {
                    ui.vertical_centered(|ui| {
                        theme::mascot(ui, 146.0);
                        ui.add_space(8.0);
                        ui.label(
                            egui::RichText::new("A lighter place to build.")
                                .size(26.0)
                                .strong()
                                .color(INK),
                        );
                        ui.add_space(7.0);
                        ui.label(
                            egui::RichText::new("Bring an idea. Feather will take it from here.")
                                .size(15.0)
                                .color(MUTED),
                        );
                        ui.add_space(20.0);
                        ui.horizontal_wrapped(|ui| {
                            for (key, label) in [
                                ("Ctrl+L", "Models"),
                                ("Shift+Tab", "Thinking"),
                                ("F1", "Shortcuts"),
                            ] {
                                egui::Frame::none()
                                    .fill(SOFT_ACCENT)
                                    .rounding(8.0)
                                    .inner_margin(egui::Margin::symmetric(9.0, 5.0))
                                    .show(ui, |ui| {
                                        ui.horizontal(|ui| {
                                            ui.label(
                                                egui::RichText::new(key)
                                                    .size(11.5)
                                                    .strong()
                                                    .color(INK),
                                            );
                                            ui.label(
                                                egui::RichText::new(label).size(11.5).color(MUTED),
                                            );
                                        });
                                    });
                            }
                        });
                    });
                },
            );
            return;
        }
        let now = self.now();
        if self.history.paged() {
            ui.horizontal(|ui| {
                if self.history.loading() {
                    ui.spinner();
                }
                if ui
                    .add_enabled(
                        self.history.remaining() > 0,
                        egui::Link::new(if self.history.loading() {
                            "Loading earlier conversation…".into()
                        } else if self.history.remaining() == 0 {
                            "Beginning of conversation".into()
                        } else {
                            format!(
                                "Load earlier conversation · {} entries",
                                self.history.remaining()
                            )
                        }),
                    )
                    .clicked()
                {
                    self.history.request_older(ui.ctx());
                }
                if let Some(error) = &self.history.error {
                    ui.label(egui::RichText::new(error).small().color(RED));
                }
            });
        }
        transcript::show(ui, &mut self.rows, now, self.show_thinking, self.running);
        let viewport = transcript::view(ui.ctx());
        self.history
            .bound_live_tail(&self.rows, viewport.at_bottom, ui.ctx());
        if !self.history.loading()
            && (viewport.scrolling_down || viewport.at_bottom)
            && self.history.evict_above(
                &mut self.rows,
                viewport.first_visible_key,
                viewport.at_bottom,
            ) > 0
        {
            cards::release_history_layouts(ui.ctx());
            ui.ctx()
                .request_discard("older redb page released; preserve viewport anchor");
        }
        if viewport.near_top && self.history.remaining() > 0 && self.history.error.is_none() {
            self.history.request_older(ui.ctx());
        }
    }
    pub(crate) fn update_ui(&mut self, ctx: &egui::Context) {
        self.preview_timing.tick(ctx);
        if ctx.input(|i| i.viewport().close_requested()) && !self.exit_ready {
            self.request_close();
        }
        // stream events
        self.drain_events();
        if self.history.poll(&mut self.rows) {
            cards::release_history_layouts(ctx);
        }
        self.proxies.poll();
        if self.exit.is_some() {
            self.shutdown_ui(ctx);
        } else {
            self.frame_ui(ctx);
        }
        self.poll_exit(ctx);
        if self.preview {
            if let Ok(path) = std::env::var("FEATHER_UI_CAPTURE") {
                self.capture_frames += 1;
                if self.capture_frames == 8
                    && std::env::var("FEATHER_UI_PREVIEW_SIZE").ok().as_deref() == Some("maximized")
                {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(true));
                }
                if self.capture_frames == 24 {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot);
                }
                let image = ctx.input(|i| {
                    i.events.iter().find_map(|e| {
                        if let egui::Event::Screenshot { image, .. } = e {
                            Some(image.clone())
                        } else {
                            None
                        }
                    })
                });
                if let Some(image) = image {
                    let pixels: Vec<u8> = image.pixels.iter().flat_map(|c| c.to_array()).collect();
                    if let Some(parent) = std::path::Path::new(&path).parent() {
                        let _ = std::fs::create_dir_all(parent);
                    }
                    if let Err(e) = image::save_buffer(
                        &path,
                        &pixels,
                        image.size[0] as u32,
                        image.size[1] as u32,
                        image::ColorType::Rgba8,
                    ) {
                        eprintln!("preview capture: {e}");
                    }
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                } else {
                    ctx.request_repaint_after(std::time::Duration::from_millis(70));
                }
            }
        }
    }

    /// Everything eframe asks us to paint for one frame: global shortcuts, the
    /// panel layout and, on top of it, the modal windows. Split out of
    /// `eframe::App::update` (which additionally pumps agent events) so the
    /// layout can be driven headlessly by tests.
    pub(crate) fn frame_ui(&mut self, ctx: &egui::Context) {
        // Winit/eframe can synchronously render the transient 199x34 Windows
        // minimized client rectangle. Never lay out whole panels in that frame;
        // update_ui still drains events and services close/restart outside here.
        if ctx.input(|i| i.viewport().minimized == Some(true))
            || ctx.screen_rect().height() < 120.0
            || ctx.screen_rect().width() < 300.0
        {
            return;
        }
        // (agent events were pumped by `update` before this call)
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::F11)) {
            let full = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(!full));
        }
        self.rewind_turns.sync(&self.input, ctx);
        self.skill_choices
            .sync(&self.input, &self.root, &self.home, ctx);
        self.skill_choices.poll();
        self.rewind_turns.poll();
        let suggestions = self.cached_suggestions();
        let modal = self.modal_open();
        let mut cursor = self.command_cursor;
        let mut selected = None;
        if !suggestions.is_empty() && !modal {
            cursor = cursor.min(suggestions.len() - 1);
            if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown)) {
                cursor = (cursor + 1) % suggestions.len();
            }
            if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp)) {
                cursor = (cursor + suggestions.len() - 1) % suggestions.len();
            }
            if ctx.input_mut(|i| {
                i.consume_key(egui::Modifiers::NONE, egui::Key::Tab)
                    || i.consume_key(egui::Modifiers::NONE, egui::Key::Enter)
            }) {
                selected = Some(suggestions[cursor].clone());
            }
        }
        drop(suggestions);
        self.command_cursor = cursor;
        if let Some(choice) = selected {
            self.accept_suggestion(choice, ctx);
        }
        // Native dialogs always offer Esc, regardless of terminal registry
        // precedence or a disabled interrupt binding. Consume it before widgets.
        if self.modal_open()
            && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
        {
            self.handle_action("app.interrupt", ctx);
        }

        // global keybindings (pi parity): resolve without cloning the event list
        let actions: Vec<(&'static str, egui::Modifiers, egui::Key)> = ctx.input(|i| {
            i.events
                .iter()
                .filter_map(|ev| match ev {
                    egui::Event::Key {
                        key,
                        modifiers,
                        pressed: true,
                        repeat: false,
                        ..
                    } => keybindings::egui_event_to_action(&self.kb, *modifiers, *key)
                        .map(|a| (a, *modifiers, *key)),
                    _ => None,
                })
                .collect()
        });
        for (action, modifiers, key) in actions {
            let modal = self.modal_open();
            let composer_focused = ctx.memory(|m| m.has_focus(egui::Id::new("feather-composer")));
            if action == "tui.input.submit" && (!composer_focused || modal) {
                continue;
            }
            if modal
                && !matches!(
                    action,
                    "app.interrupt"
                        | "tui.select.cancel"
                        | "tui.altScreen.searchClose"
                        | "app.shortcuts"
                        | "app.model.select"
                        | "app.thinking.select"
                )
            {
                continue;
            }
            if action.starts_with("app.") || action == "tui.input.submit" {
                ctx.input_mut(|i| i.consume_key(modifiers, key));
            }
            self.handle_action(action, ctx);
        }

        if self.running {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        egui::TopBottomPanel::top("feather_top")
            .show_separator_line(false)
            .resizable(false)
            .frame(egui::Frame::none().fill(PANEL).stroke(egui::Stroke::NONE))
            .show(ctx, |ui| self.top_bar(ui));
        // Header X can begin shutdown within this very frame; skip panels that
        // may synchronously read config immediately after that click.
        if self.exit.is_some() {
            return;
        }
        self.tree_panel(ctx);
        egui::TopBottomPanel::bottom("feather_input")
            .show_separator_line(false)
            .resizable(false)
            .frame(
                egui::Frame::none()
                    .fill(PANEL)
                    .stroke(egui::Stroke::NONE)
                    .inner_margin(egui::Margin::symmetric(18.0, 8.0)),
            )
            .show(ctx, |ui| self.input_bar(ui));

        if self.exit.is_some() {
            return;
        }
        egui::CentralPanel::default()
            .frame(
                egui::Frame::none()
                    .fill(PANEL)
                    .stroke(egui::Stroke::NONE)
                    .inner_margin(egui::Margin::symmetric(14.0, 8.0)),
            )
            .show(ctx, |ui| self.chat_panel(ui));

        if self.modal_open() {
            egui::Area::new(egui::Id::new("modal-scrim"))
                .order(egui::Order::Middle)
                .fixed_pos(egui::Pos2::ZERO)
                .show(ctx, |ui| {
                    let rect = ctx.screen_rect();
                    ui.allocate_rect(rect, egui::Sense::hover());
                    ui.painter().rect_filled(
                        rect,
                        0.0,
                        egui::Color32::from_rgba_unmultiplied(38, 47, 40, 82),
                    );
                });
        }
        self.resume_window(ctx);
        self.requests_window(ctx);
        self.renderer_window(ctx);
        self.models_window(ctx);
        self.thinking_picker_window(ctx);
        self.settings_window(ctx);
        self.login_window(ctx);
    }
}
