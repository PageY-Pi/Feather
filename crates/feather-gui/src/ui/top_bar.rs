//! The top bar: workspace/session identity, model chip and title-bar actions.
//!
//! Filled in by the split of `main.rs`; see `ui/mod.rs` for the rationale.

use crate::theme::{self, INK, MUTED, PANEL, SIDEBAR, TEXT};
use crate::{short_path, window_chrome, workspace, FeatherApp};
use eframe::egui;

impl FeatherApp {
    pub(crate) fn top_bar(&mut self, ui: &mut egui::Ui) {
        use window_chrome::Control;
        let (bar, _) =
            ui.allocate_exact_size(egui::vec2(ui.available_width(), 60.0), egui::Sense::hover());
        let right = bar.right() - 16.0;
        let button = |index: f32| {
            egui::Rect::from_min_size(
                egui::pos2(right - 36.0 - index * 42.0, bar.center().y - 17.0),
                egui::vec2(36.0, 34.0),
            )
        };
        let max = ui.input(|i| i.viewport().maximized.unwrap_or(false));
        let divider_x = button(2.0).left() - 22.0;
        let add_rect = egui::Rect::from_min_size(
            egui::pos2(divider_x - 122.0, bar.center().y - 17.0),
            egui::vec2(100.0, 34.0),
        );
        // The entire non-interactive header is draggable; controls are separate,
        // reserved rectangles and are never displaced by long workspace names.
        let drag_rect =
            egui::Rect::from_min_max(bar.min, egui::pos2(add_rect.left() - 8.0, bar.bottom()));
        let drag = ui.interact(
            drag_rect,
            ui.id().with("window-drag"),
            egui::Sense {
                click: true,
                drag: true,
                focusable: false,
            },
        );
        if drag.drag_started() {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
        }
        if drag.double_clicked() {
            ui.ctx()
                .send_viewport_cmd(egui::ViewportCommand::Maximized(!max));
        }
        if window_chrome::add_api(ui, add_rect).clicked() {
            self.renderer_picker = None;
            self.request_picker = None;
            self.show_login = true;
            self.model_manager.enabled = true;
            self.picker = None;
        }
        if window_chrome::control(ui, button(2.0), Control::Minimize).clicked() {
            ui.ctx()
                .send_viewport_cmd(egui::ViewportCommand::Minimized(true));
        }
        if window_chrome::control(
            ui,
            button(1.0),
            if max {
                Control::Restore
            } else {
                Control::Maximize
            },
        )
        .clicked()
        {
            ui.ctx()
                .send_viewport_cmd(egui::ViewportCommand::Maximized(!max));
        }
        if window_chrome::control(ui, button(0.0), Control::Close).clicked() {
            self.request_close();
        }
        ui.painter().line_segment(
            [
                egui::pos2(divider_x, bar.center().y - 8.0),
                egui::pos2(divider_x, bar.center().y + 8.0),
            ],
            egui::Stroke::new(1.2_f32, egui::Color32::from_rgb(170, 167, 158)),
        );
        let brand =
            egui::Rect::from_min_size(bar.min + egui::vec2(18.0, 12.0), egui::vec2(136.0, 36.0));
        ui.allocate_new_ui(
            egui::UiBuilder::new()
                .max_rect(brand)
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
            |ui| {
                ui.set_clip_rect(brand);
                theme::mascot(ui, 32.0);
                ui.label(
                    egui::RichText::new("Feather")
                        .size(20.0)
                        .strong()
                        .color(INK),
                );
            },
        );
        let path_rect = egui::Rect::from_min_max(
            egui::pos2(brand.right() + 16.0, bar.top() + 17.0),
            egui::pos2(
                (add_rect.left() - 24.0).max(brand.right() + 16.0),
                bar.bottom() - 17.0,
            ),
        );
        ui.allocate_new_ui(
            egui::UiBuilder::new()
                .max_rect(path_rect)
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
            |ui| {
                ui.set_clip_rect(path_rect);
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(short_path(&self.root))
                            .size(12.5)
                            .color(MUTED),
                    )
                    .truncate(),
                )
                .on_hover_text(self.root.display().to_string());
            },
        );
    }

    /// Left panel: the working directory tree (lazy: dirs are read on expand;
    /// clicking a file inserts an @path reference into the composer).
    pub(crate) fn tree_panel(&mut self, ctx: &egui::Context) {
        egui::SidePanel::left("feather_tree")
            .show_separator_line(false)
            .resizable(true)
            .default_width(204.0)
            .width_range(170.0..=280.0)
            .frame(
                egui::Frame::none()
                    .fill(PANEL)
                    .stroke(egui::Stroke::NONE)
                    .inner_margin(egui::Margin {
                        left: 14.0,
                        right: 4.0,
                        top: 4.0,
                        bottom: 12.0,
                    }),
            )
            .show(ctx, |ui| {
                egui::Frame::none()
                    .fill(SIDEBAR)
                    .rounding(22.0)
                    .inner_margin(egui::Margin::symmetric(14.0, 18.0))
                    .show(ui, |ui| {
                        ui.set_min_height((ui.available_height() - 4.0).max(0.0));
                        ui.label(
                            egui::RichText::new("WORKSPACE")
                                .size(11.5)
                                .strong()
                                .color(MUTED),
                        );
                        ui.add_space(3.0);
                        ui.label(
                            egui::RichText::new(
                                self.root.file_name().unwrap_or_default().to_string_lossy(),
                            )
                            .size(16.0)
                            .strong()
                            .color(TEXT),
                        );
                        ui.add_space(10.0);
                        ui.spacing_mut().item_spacing.y = 3.0;
                        egui::ScrollArea::vertical()
                            .id_salt("workspace-tree")
                            .auto_shrink([false, false])
                            .scroll_bar_visibility(
                                egui::scroll_area::ScrollBarVisibility::AlwaysVisible,
                            )
                            .show(ui, |ui| {
                                // Scope the registry-style rails to the workspace tree;
                                // other collapsing panels keep their lighter appearance.
                                ui.visuals_mut().indent_has_left_vline = true;
                                ui.visuals_mut().widgets.noninteractive.bg_stroke =
                                    egui::Stroke::new(1.0_f32, workspace::TREE_GUIDE_COLOR);
                                // Folder bodies are indented; egui paints their left rails using this local stroke.
                                let root = self.root.clone();
                                self.tree_dir(ui, &root, 0);
                            });
                    });
            });
    }

    pub(crate) fn tree_dir(&mut self, ui: &mut egui::Ui, dir: &std::path::Path, depth: u32) {
        if depth > 8 {
            return;
        }
        let listing = workspace::listing(ui.ctx(), dir);
        let dirs = &listing.dirs;
        let files = &listing.files;

        for name in dirs {
            let path = dir.join(name);
            // CollapsingHeader normally extends long labels. Give it a single-line
            // galley instead so deep/long folder names cannot widen the sidebar.
            let header_width =
                (ui.available_width() - ui.spacing().indent - ui.spacing().button_padding.x)
                    .max(0.0);
            let header = egui::WidgetText::from(egui::RichText::new(name).size(13.0).color(TEXT))
                .into_galley(
                    ui,
                    Some(egui::TextWrapMode::Truncate),
                    header_width,
                    egui::TextStyle::Button,
                );

            ui.push_id(path.to_string_lossy().to_string(), |ui| {
                egui::CollapsingHeader::new(header)
                    .default_open(depth == 0 && dirs.len() <= 6)
                    // Render the normal indented body so egui paints a vertical guide rail.
                    .show(ui, |ui| {
                        self.tree_dir(ui, &path, depth + 1);
                    })
                    .header_response
                    .on_hover_text(path.display().to_string());
            });
        }
        for name in files {
            let path = dir.join(name);
            let rel = path
                .strip_prefix(&self.root)
                .unwrap_or(&path)
                .display()
                .to_string();
            // Reserve the same disclosure-arrow column as folder headers, so
            // files and folders belonging to one parent share a text baseline.
            let response = ui
                .allocate_ui_with_layout(
                    egui::vec2(ui.available_width().max(0.0), 20.0),
                    egui::Layout::left_to_right(egui::Align::Center),
                    |ui| {
                        ui.spacing_mut().item_spacing.x = 0.0;
                        ui.add_space(ui.spacing().indent);
                        ui.add_sized(
                            [ui.available_width().max(0.0), 20.0],
                            egui::Label::new(egui::RichText::new(name).size(13.0).color(TEXT))
                                .halign(egui::Align::Min)
                                .truncate()
                                .sense(egui::Sense::click()),
                        )
                    },
                )
                .inner;
            if response
                .on_hover_cursor(egui::CursorIcon::PointingHand)
                .on_hover_text(name)
                .clicked()
            {
                self.input.push_str(&format!("@{rel} "));
            }
        }
        if dirs.len() == 200 || files.len() == 300 {
            ui.label(egui::RichText::new("…").size(11.0).color(MUTED));
        }
    }
}
