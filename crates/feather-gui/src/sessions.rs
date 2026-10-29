//! Startup session chooser: discovery, model, keyboard navigation and the
//! clickable-text-only modal. Pure data plus one egui renderer so the filter,
//! navigation and row mapping stay deterministically unit-testable.
//!
//! No durable session is created here — the picker only *describes* what the
//! workspace already has, and the app materializes a `session.redb` lazily on
//! the first submit (or immediately when the user explicitly resumes one).
#![allow(dead_code)]

use eframe::egui;
use feather_core::session_catalog::{self, SessionInfo};
use std::path::{Path, PathBuf};

use crate::dialog::{button, choice_row, error_line, footnote, search, sheet, ButtonKind, Scrim};

/// What resolving the chooser means for the app lifecycle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pick {
    /// Start fresh: stay on the in-memory welcome until the first submit.
    NewSession,
    /// Open this on-disk session (releasing the previous redb lock first).
    Resume(PathBuf),
    /// Emitted only after confirmation.
    Delete(PathBuf),
}

/// Startup decision derived from history + the `autoResumeLatest` preference.
/// It never creates anything, which is what keeps a durable empty session out
/// of the pre-choice window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartupPlan {
    /// `FEATHER_UI_PREVIEW=1`: build the offline visual fixture.
    Preview,
    /// History exists and the preference asks to continue the newest session.
    ResumeLatest,
    /// History exists: show the chooser and wait for a choice.
    Choose,
    /// No history: an agent-less welcome until the first submit.
    Welcome,
}

pub fn startup_plan(empty_history: bool, auto_resume_latest: bool, preview: bool) -> StartupPlan {
    if preview {
        StartupPlan::Preview
    } else if empty_history {
        StartupPlan::Welcome
    } else if auto_resume_latest {
        StartupPlan::ResumeLatest
    } else {
        StartupPlan::Choose
    }
}

/// Sessions for this workspace, newest activity first. Discovery failures are
/// non-fatal (the user simply sees the welcome instead of a broken window).
pub fn list(home: &Path, cwd: &Path) -> Vec<SessionInfo> {
    session_catalog::list_sessions(home, cwd).unwrap_or_default()
}

/// Unique suffix of a session id, for the compact row meta.
pub fn short_id(id: &str) -> String {
    let head: String = id
        .chars()
        .rev()
        .take(8)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    if id.chars().count() > 8 {
        format!("…{head}")
    } else {
        head
    }
}

/// Unix millis now, as `u128` to match [`SessionInfo::modified_ms`].
pub fn now_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

/// Compact "just now / 5m ago / 3h ago / 2d ago" stamp.
pub fn relative_time(modified_ms: u128, now_ms: u128) -> String {
    let secs = now_ms.saturating_sub(modified_ms) / 1000;
    if secs < 60 {
        "just now".into()
    } else if secs < 3_600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86_400 {
        format!("{}h ago", secs / 3_600)
    } else {
        format!("{}d ago", secs / 86_400)
    }
}

/// Row 0 is always "Start a new session"; rows 1.. are the filtered history,
/// so selecting the first history row is exactly "continue latest".
pub const NEW_ROW: usize = 0;

pub struct SessionChooser {
    pub sessions: Vec<SessionInfo>,
    /// Live search text (bound to the single-line field).
    pub filter: String,
    /// Row index into `0..row_count()`.
    pub cursor: usize,
    pub pending_delete: Option<PathBuf>,
    pub error: Option<String>,
}

impl SessionChooser {
    pub fn new(sessions: Vec<SessionInfo>) -> Self {
        // Continue-latest is the sensible default: the newest session is row 1.
        let cursor = if sessions.is_empty() { NEW_ROW } else { 1 };
        Self {
            sessions,
            filter: String::new(),
            cursor,
            pending_delete: None,
            error: None,
        }
    }
    pub fn request_delete(&mut self, row: usize) {
        if let Some(index) = self.session_at_row(row) {
            self.pending_delete = Some(self.sessions[index].dir.clone());
        }
    }
    pub fn confirm_delete(&mut self) -> Option<Pick> {
        self.pending_delete.take().map(Pick::Delete)
    }

    /// Indices into `sessions` matching the current filter, order preserved.
    pub fn visible(&self) -> Vec<usize> {
        let query = self.filter.trim().to_lowercase();
        self.sessions
            .iter()
            .enumerate()
            .filter(|(_, info)| {
                query.is_empty()
                    || info.first_prompt.to_lowercase().contains(&query)
                    || info.id.to_lowercase().contains(&query)
            })
            .map(|(index, _)| index)
            .collect()
    }

    pub fn row_count(&self) -> usize {
        1 + self.visible().len()
    }

    /// Session index for a row, or `None` for the synthetic new-session row.
    pub fn session_at_row(&self, row: usize) -> Option<usize> {
        if row == NEW_ROW {
            return None;
        }
        self.visible().get(row - 1).copied()
    }

    /// The row the highlight and Enter target (clamped to the visible range).
    pub fn active_row(&self) -> usize {
        self.cursor.min(self.row_count().saturating_sub(1))
    }

    /// Wrap-around vertical navigation over the visible rows.
    pub fn move_cursor(&mut self, delta: isize) {
        let rows = self.row_count();
        if rows == 0 {
            self.cursor = NEW_ROW;
            return;
        }
        let current = self.active_row() as isize;
        self.cursor = (current + delta).rem_euclid(rows as isize) as usize;
    }

    pub fn selected_pick(&self) -> Pick {
        match self.session_at_row(self.active_row()) {
            Some(index) => Pick::Resume(self.sessions[index].dir.clone()),
            None => Pick::NewSession,
        }
    }

    /// Replace the filter and reset the highlight to the newest match.
    pub fn set_filter(&mut self, filter: impl Into<String>) {
        self.filter = filter.into();
        self.cursor = if self.row_count() > 1 { 1 } else { NEW_ROW };
    }

    /// (title, meta) for a row, used by the renderer and by unit tests.
    pub fn row_text(&self, row: usize) -> (String, String) {
        if row == NEW_ROW {
            return ("Start a new session".into(), "Ctrl+Shift+N".into());
        }
        match self.session_at_row(row) {
            Some(index) => {
                let info = &self.sessions[index];
                let title = if info.first_prompt.trim().is_empty() {
                    "(no prompt yet)".into()
                } else {
                    info.first_prompt.clone()
                };
                let meta = format!(
                    "{} · {}",
                    short_id(&info.id),
                    relative_time(info.modified_ms, now_ms())
                );
                (title, meta)
            }
            None => (String::new(), String::new()),
        }
    }
}

/// The resume chooser as a Feather sheet: a search field pinned under the title,
/// a scrolling list of hairline-separated rows, and a delete confirmation that
/// takes over the footer. Keyboard is ↑/↓ + Enter, Del asks, Esc closes.
pub fn picker_window(
    ctx: &egui::Context,
    chooser: &mut SessionChooser,
    open: &mut bool,
) -> Option<Pick> {
    let mut pick = None;
    let rows = chooser.row_count();
    let confirming = chooser.pending_delete.is_some();

    // Keys are read once, before the regions are built, so a key handled by the
    // search field never also moves the cursor.
    let (down, up, enter, delete) = ctx.input_mut(|i| {
        (
            i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown),
            i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp),
            i.consume_key(egui::Modifiers::NONE, egui::Key::Enter),
            i.consume_key(egui::Modifiers::NONE, egui::Key::Delete),
        )
    });
    if confirming {
        if enter {
            pick = chooser.confirm_delete();
        }
    } else if rows > 0 {
        if down {
            chooser.move_cursor(1);
        }
        if up {
            chooser.move_cursor(-1);
        }
        if delete {
            chooser.request_delete(chooser.active_row());
        } else if enter {
            pick = Some(chooser.selected_pick());
        }
    }
    let highlight = chooser.active_row();
    // The card's regions are closures, so everything the rows need is snapshotted
    // first and every intent is recorded rather than applied inside the draw.
    let listed: Vec<(String, String)> = (0..rows).map(|row| chooser.row_text(row)).collect();
    let mut filter = std::mem::take(&mut chooser.filter);
    let error = chooser.error.clone();
    let mut hover_row = None;
    let mut click_row = None;
    let mut clicked_primary = false;
    let mut clicked_confirm = false;
    let mut clicked_abort = false;

    sheet(ctx, "session-chooser", "Resume a session")
        .width(520.0)
        .scrim(Scrim::Soft)
        .lead(if confirming {
            "This cannot be undone."
        } else {
            "Type to search · ↑ / ↓ choose · Enter opens · Del deletes"
        })
        .top(|ui| {
            search(ui, "Search session history…", &mut filter, "session-history");
        })
        .body(|ui| {
            for (row, (title, meta)) in listed.iter().enumerate() {
                let selected = row == highlight;
                let response = choice_row(ui, title, meta, selected, false);
                if selected && (down || up) {
                    response.scroll_to_me(Some(egui::Align::Center));
                }
                if confirming {
                    continue;
                }
                if response.hovered()
                    && ui.input(|i| {
                        i.events
                            .iter()
                            .any(|e| matches!(e, egui::Event::PointerMoved(_)))
                    })
                {
                    hover_row = Some(row);
                }
                if response.clicked() {
                    click_row = Some(row);
                }
            }
        })
        .bottom(|ui| {
            if confirming {
                ui.horizontal(|ui| {
                    clicked_confirm = button(ui, "Confirm delete", ButtonKind::Danger).clicked();
                    clicked_abort = button(ui, "Cancel", ButtonKind::Ghost).clicked();
                });
                ui.add_space(6.0);
                error_line(ui, "Permanently delete this session and its history?");
            } else {
                if let Some(e) = &error {
                    error_line(ui, e);
                    ui.add_space(6.0);
                }
                ui.horizontal(|ui| {
                    clicked_primary = button(ui, "Start a new session", ButtonKind::Primary).clicked();
                    footnote(ui, "Esc keeps the welcome fresh");
                });
            }
        })
        .show(open);

    chooser.filter = filter;
    if let Some(row) = hover_row {
        chooser.cursor = row;
    }
    if let Some(row) = click_row {
        chooser.cursor = row;
        pick = Some(chooser.selected_pick());
    }
    if clicked_primary {
        pick = Some(Pick::NewSession);
    }
    if clicked_abort {
        chooser.pending_delete = None;
    }
    if clicked_confirm {
        pick = chooser.confirm_delete();
    }
    pick
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deletion_requires_request_and_confirmation_and_respects_filter() {
        let mut chooser = chooser();
        assert!(chooser.confirm_delete().is_none());
        chooser.request_delete(NEW_ROW);
        assert!(chooser.pending_delete.is_none());
        chooser.set_filter("docs");
        chooser.request_delete(1);
        assert_eq!(
            chooser.pending_delete,
            Some(PathBuf::from("sessions/s-middle"))
        );
        assert_eq!(
            chooser.confirm_delete(),
            Some(Pick::Delete(PathBuf::from("sessions/s-middle")))
        );
        assert!(chooser.confirm_delete().is_none());
        // Request is an intent only: nothing removed before the app confirms IO.
        assert_eq!(chooser.sessions.len(), 3);
    }
    fn info(id: &str, prompt: &str, modified_ms: u128) -> SessionInfo {
        SessionInfo {
            id: id.into(),
            dir: PathBuf::from(format!("sessions/{id}")),
            modified_ms,
            first_prompt: prompt.into(),
            created_ms: modified_ms,
            cwd: PathBuf::from("workspace"),
        }
    }

    fn chooser() -> SessionChooser {
        SessionChooser::new(vec![
            info("s-newest", "fix the parser", 300),
            info("s-middle", "write docs", 200),
            info("s-oldest", "ship release", 100),
        ])
    }

    #[test]
    fn startup_plan_never_materializes_a_session() {
        assert_eq!(startup_plan(true, false, false), StartupPlan::Welcome);
        assert_eq!(startup_plan(true, true, false), StartupPlan::Welcome);
        assert_eq!(startup_plan(false, false, false), StartupPlan::Choose);
        assert_eq!(startup_plan(false, true, false), StartupPlan::ResumeLatest);
        // the preview fixture always wins, even with history on disk
        assert_eq!(startup_plan(false, false, true), StartupPlan::Preview);
        assert_eq!(startup_plan(false, true, true), StartupPlan::Preview);
    }

    #[test]
    fn rows_put_new_first_then_newest_first() {
        let chooser = chooser();
        assert_eq!(chooser.row_count(), 4);
        assert_eq!(chooser.session_at_row(NEW_ROW), None);
        assert_eq!(chooser.row_text(NEW_ROW).0, "Start a new session");
        assert_eq!(chooser.session_at_row(1), Some(0));
        assert_eq!(chooser.session_at_row(2), Some(1));
        assert_eq!(chooser.session_at_row(3), Some(2));
        assert_eq!(chooser.row_text(1).0, "fix the parser");
        // the default highlight lands on the newest session = continue latest
        assert_eq!(chooser.active_row(), 1);
        assert_eq!(
            chooser.selected_pick(),
            Pick::Resume(PathBuf::from("sessions/s-newest"))
        );
    }

    #[test]
    fn filter_narrows_rows_and_keeps_new_row() {
        let mut chooser = chooser();
        chooser.set_filter("docs");
        assert_eq!(chooser.row_count(), 2);
        assert_eq!(chooser.row_text(1).0, "write docs");
        chooser.set_filter("S-OLD");
        assert_eq!(chooser.row_count(), 2);
        assert_eq!(chooser.row_text(1).0, "ship release");
        chooser.set_filter("nothing matches");
        assert_eq!(chooser.row_count(), 1);
        assert_eq!(chooser.active_row(), NEW_ROW);
        assert_eq!(chooser.selected_pick(), Pick::NewSession);
        chooser.set_filter("");
        assert_eq!(chooser.row_count(), 4);
    }

    #[test]
    fn navigation_wraps_both_ways_and_respects_filter() {
        let mut chooser = chooser();
        // start on the newest session (row 1); walk down past the oldest
        chooser.move_cursor(1);
        assert_eq!(chooser.active_row(), 2);
        chooser.move_cursor(1);
        assert_eq!(chooser.active_row(), 3);
        chooser.move_cursor(1);
        assert_eq!(
            chooser.active_row(),
            NEW_ROW,
            "wraps to the new-session row"
        );
        chooser.move_cursor(-1);
        assert_eq!(chooser.active_row(), 3, "wraps backward to the oldest");
        chooser.set_filter("docs");
        // rows: [New, write docs]; the highlight resets to the newest match
        assert_eq!(chooser.row_text(chooser.active_row()).0, "write docs");
        chooser.move_cursor(1);
        assert_eq!(chooser.active_row(), NEW_ROW);
        chooser.move_cursor(-1);
        assert_eq!(chooser.row_text(chooser.active_row()).0, "write docs");
    }

    #[test]
    fn empty_history_has_only_the_new_row() {
        let mut chooser = SessionChooser::new(Vec::new());
        assert_eq!(chooser.row_count(), 1);
        assert_eq!(chooser.selected_pick(), Pick::NewSession);
        chooser.move_cursor(1);
        assert_eq!(chooser.active_row(), NEW_ROW);
    }

    #[test]
    fn relative_time_is_stable_across_buckets() {
        let now = 1_000_000_000_000u128;
        assert_eq!(relative_time(now, now), "just now");
        assert_eq!(relative_time(now - 5 * 60 * 1000, now), "5m ago");
        assert_eq!(relative_time(now - 3 * 3600 * 1000, now), "3h ago");
        assert_eq!(relative_time(now - 2 * 86_400 * 1000, now), "2d ago");
    }
}
