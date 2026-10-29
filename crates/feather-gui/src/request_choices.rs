//! Small session/model-style choosers for lazy skills and process-local proxies.
use crate::theme::*;
use eframe::egui;
use crate::dialog::{button, choice_row, error_line, field, footnote, hint, search, sheet, tag, ButtonKind, Scrim};
use feather::proxy::{self, Catalog};
use feather_core::provider::transport::{self, Route};
use std::{path::PathBuf, sync::mpsc};

pub enum Picker {
    Skills {
        resources: feather::resources::ResourceLoad,
        filter: String,
        cursor: usize,
    },
    Proxy {
        address: String,
        cursor: usize,
        changed: bool,
    },
    /// Provider traffic ceiling: one number, typed or chosen from presets.
    RateLimit { input: String, selected: usize },
}
pub enum Choice {
    Skill(String),
    Proxy(Route),
    Remove(Route),
    RateLimit(u64),
    Error(String),
    Scan,
}
#[derive(Default)]
pub struct Proxies {
    pub catalog: Catalog,
    pending: Option<mpsc::Receiver<Catalog>>,
    scan_route: Option<Route>,
    pub error: Option<String>,
}
impl Proxies {
    pub fn new(home: &std::path::Path) -> Self {
        let catalog = proxy::load(home);
        transport::set_route(catalog.selected.clone());
        Self {
            catalog,
            pending: None,
            scan_route: None,
            error: None,
        }
    }
    pub fn scan(&mut self, home: PathBuf, ctx: egui::Context) {
        if self.pending.is_some() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        self.pending = Some(rx);
        self.scan_route = Some(self.catalog.selected.clone());
        std::thread::spawn(move || {
            let found = proxy::discover_for(&home);
            let catalog = proxy::merge_discovered(&home, found);
            let _ = tx.send(catalog);
            ctx.request_repaint();
        });
    }
    pub fn poll(&mut self) {
        let Some(rx) = &self.pending else {
            return;
        };
        if let Ok(mut catalog) = rx.try_recv() {
            if self.scan_route.as_ref() != Some(&self.catalog.selected) {
                catalog.selected = self.catalog.selected.clone();
            }
            self.scan_route = None;
            self.error = catalog.error.clone();
            self.catalog = catalog;
            transport::set_route(self.catalog.selected.clone());
            self.pending = None;
        }
    }
    pub fn scanning(&self) -> bool {
        self.pending.is_some()
    }
    pub fn label(&self) -> String {
        match &self.catalog.selected {
            Route::Direct => "Proxy: Off".into(),
            Route::Proxy { url } => format!(
                "Proxy: {}",
                url.strip_prefix("http://")
                    .or_else(|| url.strip_prefix("socks5h://"))
                    .unwrap_or(url)
            ),
        }
    }
}

/// Provider traffic limit state for the status bar and its popup.
#[derive(Default)]
pub struct Limits {
    pub limit: feather::netlimit::Limit,
    pub error: Option<String>,
}
impl Limits {
    pub fn new(home: &std::path::Path) -> Self {
        let limit = feather::netlimit::load(home);
        // Push the persisted value into the shaper at startup so the limit is
        // already in force for the first request.
        limit.apply();
        Self { limit, error: None }
    }
    pub fn label(&self) -> String {
        self.limit.label()
    }
    /// Store a typed value. Success clears the error; failure keeps the
    /// previous limit active and reports why.
    pub fn save(&mut self, home: &std::path::Path, bytes_per_second: u64) {
        match feather::netlimit::save(home, bytes_per_second) {
            Ok(limit) => {
                self.limit = limit;
                self.error = None;
            }
            Err(e) => self.error = Some(e),
        }
    }
}
pub fn window(
    ctx: &egui::Context,
    picker: &mut Picker,
    proxies: &Proxies,
    limits: &Limits,
    open: &mut bool,
) -> Option<Choice> {
    // Keys are read once, before the card is built, so a field that owns the
    // keyboard keeps its own keys and Escape never leaks past a text edit.
    let typing = ctx.wants_keyboard_input();
    let (escape, up, down, enter, delete) = ctx.input_mut(|i| {
        (
            i.consume_key(egui::Modifiers::NONE, egui::Key::Escape),
            i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp),
            i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown),
            i.consume_key(egui::Modifiers::NONE, egui::Key::Enter),
            i.consume_key(egui::Modifiers::NONE, egui::Key::Delete),
        )
    });
    if escape && !typing {
        *open = false;
        return None;
    }

    match picker {
        Picker::Skills {
            resources,
            filter,
            cursor,
        } => {
            let query = filter.to_lowercase();
            let rows: Vec<(String, String)> = resources
                .skills
                .iter()
                .filter(|s| {
                    s.name.to_lowercase().contains(&query)
                        || s.description.to_lowercase().contains(&query)
                })
                .map(|s| (s.name.clone(), s.description.clone()))
                .collect();
            if !rows.is_empty() {
                *cursor = (*cursor).min(rows.len() - 1);
                if down {
                    *cursor = (*cursor + 1) % rows.len();
                }
                if up {
                    *cursor = (*cursor + rows.len() - 1) % rows.len();
                }
            }
            let highlight = *cursor;
            let keyboard_pick = (enter && !typing && !rows.is_empty())
                .then(|| rows[highlight].0.clone());
            let diagnostics: Vec<String> = resources
                .diagnostics
                .iter()
                .take(3)
                .map(|d| d.message.clone())
                .collect();
            let mut chosen: Option<String> = None;
            let mut hover = None;

            sheet(ctx, "skill-picker", "Choose a skill")
                .width(480.0)
                .scrim(Scrim::Soft)
                .lead("Only descriptions are listed. Instructions load when invoked.")
                .top(|ui| {
                    search(ui, "Search skills…", filter, "skill-search");
                    if rows.is_empty() {
                        ui.add_space(8.0);
                        hint(ui, "No skills found. Add .feather/skills/<name>/SKILL.md.");
                    }
                })
                .body(|ui| {
                    for (index, (name, description)) in rows.iter().enumerate() {
                        let response = choice_row(ui, name, description, index == highlight, false);
                        if index == highlight && (up || down) {
                            response.scroll_to_me(Some(egui::Align::Center));
                        }
                        if response.hovered() {
                            hover = Some(index);
                        }
                        if response.clicked() {
                            chosen = Some(name.clone());
                        }
                    }
                })
                .bottom(|ui| {
                    for message in &diagnostics {
                        hint(ui, message);
                    }
                    footnote(ui, "↑ / ↓ choose · Enter runs · Esc closes");
                })
                .show(open);

            if let Some(index) = hover {
                *cursor = index;
            }
            keyboard_pick.or(chosen).map(Choice::Skill)
        }
        Picker::Proxy {
            address,
            cursor,
            changed,
        } => {
            let routes: Vec<Route> = std::iter::once(Route::Direct)
                .chain(
                    proxies
                        .catalog
                        .entries
                        .iter()
                        .map(|e| e.endpoint.route.clone()),
                )
                .collect();
            *cursor = (*cursor).min(routes.len() - 1);
            if down {
                *cursor = (*cursor + 1) % routes.len();
            }
            if up {
                *cursor = (*cursor + routes.len() - 1) % routes.len();
            }
            let highlight = *cursor;
            let removable = routes
                .get(highlight)
                .filter(|route| matches!(route, Route::Proxy { .. }))
                .cloned();
            let labels: Vec<(String, String)> = routes
                .iter()
                .enumerate()
                .map(|(n, route)| {
                    if n == 0 {
                        ("Off · direct connection".to_string(), String::new())
                    } else {
                        let entry = &proxies.catalog.entries[n - 1];
                        (
                            route.to_string(),
                            if entry.active { "detected" } else { "saved" }.to_string(),
                        )
                    }
                })
                .collect();
            let current = proxies.catalog.selected.clone();
            let scanning = proxies.scanning();
            let proxy_error = proxies.error.clone();
            let mut chosen: Option<Choice> = None;
            let mut apply_address = false;
            let mut hover = None;
            let mut remove = None;
            let mut scan = false;

            sheet(ctx, "proxy-picker", "Choose proxy")
                .width(480.0)
                .scrim(Scrim::Soft)
                .lead("Applies to Feather requests only. No VPN or system settings change.")
                .top(|ui| {
                    for (index, (label, meta)) in labels.iter().enumerate() {
                        let active = routes[index] == current;
                        let response = choice_row(ui, label, meta, index == highlight, active);
                        if index == highlight && (up || down) {
                            response.scroll_to_me(Some(egui::Align::Center));
                        }
                        if response.hovered() {
                            hover = Some(index);
                        }
                        if response.clicked() {
                            chosen = Some(Choice::Proxy(routes[index].clone()));
                        }
                    }
                })
                .body(|ui| {
                    ui.add_space(8.0);
                    if button(
                        ui,
                        if scanning {
                            "Scanning localhost…"
                        } else {
                            "Scan common proxy ports"
                        },
                        ButtonKind::Ghost,
                    )
                    .clicked()
                    {
                        scan = true;
                    }
                    ui.add_space(6.0);
                    field(ui, "proxy-address", address, "http://127.0.0.1:7890 or socks5h://…");
                    if let Some(error) = &proxy_error {
                        ui.add_space(6.0);
                        error_line(ui, error);
                    }
                })
                .bottom(|ui| {
                    if let Some(route) = &removable {
                        if button(ui, "Remove saved proxy", ButtonKind::Danger).clicked() {
                            remove = Some(route.clone());
                        }
                        ui.add_space(6.0);
                    }
                    // The typed address wins over the highlighted row; a
                    // malformed one reports instead of picking a proxy.
                    if button(ui, "Add and use", ButtonKind::Primary).clicked() {
                        apply_address = true;
                    }
                    footnote(ui, "↑ / ↓ choose · Del removes · Esc closes");
                })
                .show(open);

            let _ = changed;
            if let Some(index) = hover {
                *cursor = index;
            }
            if !typing && enter && chosen.is_none() {
                chosen = Some(Choice::Proxy(routes[highlight].clone()));
            }
            if !typing && delete && removable.is_some() {
                remove = removable.clone();
            }
            if scan {
                return Some(Choice::Scan);
            }
            if remove.is_some() {
                return remove.map(Choice::Remove);
            }
            if apply_address {
                return Some(match Route::proxy(address.as_str()) {
                    Ok(route) => Choice::Proxy(route),
                    Err(e) => Choice::Error(e.to_string()),
                });
            }
            chosen
        }
        Picker::RateLimit { input, selected } => {
            if *selected != usize::MAX {
                *selected = (*selected).min(feather::netlimit::SUGGESTED.len() - 1);
            }
            let current = &limits.limit;
            let current_label = if current.bytes_per_second == 0 {
                "Off · no limit".to_string()
            } else {
                format!(
                    "{} per second",
                    feather::netlimit::human(current.bytes_per_second)
                )
            };
            let presets: Vec<String> = feather::netlimit::SUGGESTED
                .iter()
                .enumerate()
                .map(|(n, (label, _))| {
                    format!("{label}{}", if n == 0 { " · no limit" } else { "" })
                })
                .collect();
            let highlight = *selected;
            let rate_error = limits.error.clone();
            let mut chosen_preset = None;
            let mut hover = None;
            let mut apply = None;

            sheet(ctx, "rate-limit", "Network rate limit")
                .width(480.0)
                .scrim(Scrim::Soft)
                .lead("Caps the bandwidth Feather uses for model requests and replies.")
                .top(|ui| {
                    tag(ui, &format!("now {current_label}"), TEAL);
                    ui.add_space(8.0);
                    for (index, label) in presets.iter().enumerate() {
                        let response = choice_row(ui, label, "", index == highlight, false);
                        if index == highlight && (up || down) {
                            response.scroll_to_me(Some(egui::Align::Center));
                        }
                        if response.hovered() {
                            hover = Some(index);
                        }
                        if response.clicked() {
                            chosen_preset = Some(index);
                        }
                    }
                })
                .body(|ui| {
                    ui.add_space(8.0);
                    hint(ui, "Or type a limit: 256 KB/s · 1.5 MB · 20 MB · Off (B, KB, MB, GB)");
                    ui.add_space(4.0);
                    field(ui, "rate-limit-value", input, "512 KB/s");
                    if let Some(error) = &rate_error {
                        ui.add_space(6.0);
                        error_line(ui, error);
                    }
                })
                .bottom(|ui| {
                    let pressed = button(ui, "Apply limit", ButtonKind::Primary).clicked()
                        || (!typing && enter);
                    if pressed {
                        apply = Some(());
                    }
                    footnote(ui, "Applies from the next request");
                })
                .show(open);

            if let Some(index) = chosen_preset {
                *selected = index;
                *input = feather::netlimit::SUGGESTED[index].0.to_string();
            } else if let Some(index) = hover {
                *selected = index;
            }
            apply.map(|_| match feather::netlimit::parse(input) {
                Ok(bytes) => Choice::RateLimit(bytes),
                Err(message) => Choice::Error(message),
            })
        }
    }
}
