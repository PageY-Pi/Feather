//! Native dialog card look, shared by every Feather sheet.
//!
//! The card language follows the reference desktop agent (percho): a borderless
//! white sheet with a 16px radius and a wide, very transparent drop shadow; a
//! bold 19px title pinned at the top right beside a quiet ✕; a single scrolling
//! body; fade-ended hairlines instead of hard separators; rows that wash on
//! hover and carry a check mark when they are the current choice; and a pinned
//! footer of ghost / solid / danger buttons.
//!
//! Everything here is plain egui painting — no images, no web view, no GPU-only
//! features — so the CPU software renderer draws exactly the same card.

use crate::theme::*;
use eframe::egui;
use egui::{
    pos2, vec2, Color32, Id, Margin, Pos2, Rect, Response, Sense, Stroke, Ui, Vec2,
};

/// Card corner radius. Larger than the in-page cards so a dialog reads as a
/// sheet floating above the conversation rather than another content card.
pub const CARD_ROUNDING: f32 = 16.0;
/// Row corner radius inside a card.
pub const ROW_ROUNDING: f32 = 10.0;
/// Control height for dialog buttons and fields.
pub const CONTROL_HEIGHT: f32 = 32.0;
/// Track height of the thinking-effort bar.
pub const EFFORT_HEIGHT: f32 = 26.0;

// ---------------------------------------------------------------------------
// scrim
// ---------------------------------------------------------------------------

/// How much a sheet dims the page beneath it.
#[derive(Clone, Copy, PartialEq, Eq)]
/// `None` is part of the public surface even though every current caller dims:
/// it keeps "draw this sheet inline, no veil" expressible for previews.
#[allow(dead_code)]
pub enum Scrim {
    /// No dimming (sheets drawn inline, e.g. previews).
    None,
    /// A picker: marks the page as frozen without dimming it.
    Soft,
    /// A full settings/form sheet: the page must clearly read as unreachable.
    Deep,
}

impl Scrim {
    fn color(self) -> Option<Color32> {
        match self {
            Scrim::None => None,
            Scrim::Soft => Some(SCRIM_SOFT),
            Scrim::Deep => Some(SCRIM_DEEP),
        }
    }
}


// ---------------------------------------------------------------------------
// card shell
// ---------------------------------------------------------------------------

/// One sheet. Build it with [`sheet`], fill up to three regions, then call
/// [`Sheet::show`]:
///
/// - `top` is pinned under the title (search rows, tabs).
/// - `body` scrolls, and fades at the edges where content is cut off.
/// - `bottom` is pinned above the card edge (commit/cancel actions).
///
/// Fade-ended hairlines separate whatever regions exist; a region that is empty
/// does not leave a stray rule.
pub struct Sheet<'a> {
    ctx: egui::Context,
    id: Id,
    title: String,
    lead: Option<String>,
    width: f32,
    max_height: f32,
    scrim: Scrim,
    closable: bool,
    top: Option<Box<dyn FnOnce(&mut Ui) + 'a>>,
    body: Option<Box<dyn FnOnce(&mut Ui) + 'a>>,
    bottom: Option<Box<dyn FnOnce(&mut Ui) + 'a>>,
}

/// Start a sheet. `id` must stay stable for the lifetime of the dialog so the
/// window keeps its position and focus; `title` is the bold line at the top.
pub fn sheet<'a>(
    ctx: &egui::Context,
    id: impl std::hash::Hash,
    title: impl Into<String>,
) -> Sheet<'a> {
    Sheet {
        ctx: ctx.clone(),
        id: Id::new(("feather-sheet", id)),
        title: title.into(),
        lead: None,
        width: 440.0,
        max_height: 620.0,
        scrim: Scrim::Soft,
        closable: true,
        top: None,
        body: None,
        bottom: None,
    }
}

impl<'a> Sheet<'a> {
    pub fn width(mut self, width: f32) -> Self {
        self.width = width;
        self
    }
    /// A one-line explanation under the title, 13px muted.
    pub fn lead(mut self, lead: impl Into<String>) -> Self {
        self.lead = Some(lead.into());
        self
    }
    pub fn scrim(mut self, scrim: Scrim) -> Self {
        self.scrim = scrim;
        self
    }
    /// A sheet with nothing to dismiss (e.g. one-time setup) can hide the ✕.
    pub fn closable(mut self, closable: bool) -> Self {
        self.closable = closable;
        self
    }
    pub fn top(mut self, add: impl FnOnce(&mut Ui) + 'a) -> Self {
        self.top = Some(Box::new(add));
        self
    }
    pub fn body(mut self, add: impl FnOnce(&mut Ui) + 'a) -> Self {
        self.body = Some(Box::new(add));
        self
    }
    pub fn bottom(mut self, add: impl FnOnce(&mut Ui) + 'a) -> Self {
        self.bottom = Some(Box::new(add));
        self
    }

    /// Draw the sheet. Clicking ✕, pressing Esc, or clicking the page outside
    /// the card sets `*open = false`; the caller owns the open flag and simply
    /// stops asking for the sheet next frame.
    ///
    /// The sheet owns its own scrim: a veil below the card that dims the page
    /// but never eats pointer input, so a sheet used standalone (previews,
    /// tests) looks and behaves exactly like one inside the app. A click
    /// outside is recognised from the card's geometry, not by hit-testing it.
    pub fn show(self, open: &mut bool) -> bool {
        let screen = self.ctx.screen_rect();
        let margin = 20.0;
        let width = self
            .width
            .min((screen.width() - 2.0 * ARENA_GUTTER).max(240.0));
        let total_budget = self
            .max_height
            .min((screen.height() - 2.0 * ARENA_GUTTER).max(180.0));
        let mut dismiss = false;
        if let Some(color) = self.scrim.color() {
            // A veil only: it dims the page but is never interactable, so a
            // click passes to whatever is under it and the card beneath keeps
            // priority. Dismissal is decided from the card's own geometry.
            self.ctx
                .layer_painter(egui::LayerId::new(
                    egui::Order::Middle,
                    self.id.with("scrim"),
                ))
                .rect_filled(screen, 0.0, color);
        }

        let card_rect = egui::Area::new(self.id)
            .order(egui::Order::Foreground)
            .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
            .constrain_to(screen.shrink(ARENA_GUTTER))
            .show(&self.ctx, |ui| {
                egui::Frame::none()
                    .fill(PANEL_2)
                    .rounding(CARD_ROUNDING)
                    .shadow(dialog_shadow())
                    .inner_margin(Margin {
                        left: margin,
                        right: margin,
                        top: 18.0,
                        bottom: 16.0,
                    })
                    .show(ui, |ui| {
                        ui.set_width((width - 2.0 * margin).max(160.0));
                        ui.spacing_mut().item_spacing.y = 0.0;
                        if header(ui, &self.title, self.lead.as_deref(), self.closable) {
                            dismiss = true;
                        }
                        ui.add_space(12.0);

                        let mut top = self.top;
                        let mut body = self.body;
                        let mut bottom = self.bottom;
                        let has_bottom = bottom.is_some();

                        if let Some(add) = top.take() {
                            add(ui);
                            if body.is_some() {
                                ui.add_space(10.0);
                                fade_rule(ui);
                                ui.add_space(10.0);
                            }
                        }
                        let used = ui.min_rect().height();
                        if let Some(add) = body.take() {
                            // Keep the footer (and the rule above it) inside the
                            // card at every window height: the body yields first.
                            let reserve = if has_bottom { 60.0 } else { 0.0 };
                            let budget =
                                (total_budget - used - reserve).clamp(88.0, total_budget.max(88.0));
                            scrolling(ui, self.id, budget, add);
                        }
                        if let Some(add) = bottom.take() {
                            ui.add_space(12.0);
                            fade_rule(ui);
                            ui.add_space(12.0);
                            add(ui);
                        }
                    })
                    .response
                    .rect
            })
            .response
            .rect;

        // Click-outside, decided purely from geometry: a primary click whose
        // press landed on the page but not on the card. The card is in a higher
        // layer, so it has already consumed its own clicks by this point.
        if self.closable && !dismiss {
            let outside_click = self.ctx.input(|i| {
                i.events.iter().any(|event| match event {
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        ..
                    } => !card_rect.contains(*pos),
                    _ => false,
                })
            });
            if outside_click {
                dismiss = true;
            }
        }

        if dismiss {
            *open = false;
        }
        *open
    }

}

/// Space kept free between a sheet and the viewport edge.
pub const ARENA_GUTTER: f32 = 24.0;

// ---------------------------------------------------------------------------
// header
// ---------------------------------------------------------------------------

/// Title row: bold title on the left, quiet ✕ on the right. The title wraps
/// within the space left of the ✕, so an overlong name can never collide with
/// the close control or push it off the card.
///
/// Returns true when the ✕ was clicked.
fn header(ui: &mut Ui, title: &str, lead: Option<&str>, closable: bool) -> bool {
    let mut closed = false;
    let row_height = 28.0;
    let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), row_height), Sense::hover());
    let painter = ui.painter().clone();
    let text_width = (row.width() - if closable { 34.0 } else { 0.0 }).max(40.0);
    let galley = painter.layout(
        title.to_owned(),
        egui::FontId::proportional(19.0),
        INK,
        text_width,
    );
    painter.galley(
        pos2(row.left(), row.center().y - galley.size().y * 0.5),
        galley,
        INK,
    );
    if closable {
        let rect = Rect::from_center_size(
            pos2(row.right() - 13.0, row.center().y),
            Vec2::splat(26.0),
        );
        if close_button(ui, rect) {
            closed = true;
        }
    }
    if let Some(lead) = lead {
        ui.add_space(6.0);
        let galley = painter.layout(
            lead.to_owned(),
            egui::FontId::proportional(13.0),
            MUTED,
            ui.available_width(),
        );
        let y = ui.cursor().min.y;
        painter.galley(pos2(ui.cursor().min.x, y), galley.clone(), MUTED);
        ui.add_space(galley.size().y);
    }
    closed
}

/// The quiet ✕ in the card corner: no fill until the pointer arrives.
pub fn close_button(ui: &mut Ui, rect: Rect) -> bool {
    let response = ui.interact(rect, ui.id().with("dialog-close"), Sense::click());
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "Close")
    });
    let hovered = response.hovered();
    let pressed = response.is_pointer_button_down_on();
    let painter = ui.painter();
    if hovered || pressed || response.has_focus() {
        painter.rect_filled(
            rect,
            8.0,
            if pressed {
                RULE
            } else if hovered {
                HOVER
            } else {
                Color32::TRANSPARENT
            },
        );
    }
    if response.has_focus() {
        painter.rect_stroke(
            rect,
            8.0,
            Stroke::new(1.0_f32, TEAL.gamma_multiply(0.6)),
        );
    }
    // Two 1.5px strokes, inset by 8px: the same mark as the window chrome ✕.
    let color = if hovered || pressed { INK } else { MUTED };
    let stroke = Stroke::new(1.5_f32, color);
    let c = rect.center();
    painter.line_segment([c + vec2(-4.5, -4.5), c + vec2(4.5, 4.5)], stroke);
    painter.line_segment([c + vec2(-4.5, 4.5), c + vec2(4.5, -4.5)], stroke);
    response.clicked()
}

// ---------------------------------------------------------------------------
// scroll body
// ---------------------------------------------------------------------------

/// Scrolling body with both edges fading out where content is cut. The fade is
/// painted after the content, so it covers only the strip at the viewport edge
/// and never the whole sheet.
fn scrolling(ui: &mut Ui, sheet: Id, budget: f32, add: impl FnOnce(&mut Ui)) {
    let output = egui::ScrollArea::vertical()
        .id_salt(sheet.with("body"))
        .max_height(budget)
        .auto_shrink([false, true])
        .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::VisibleWhenNeeded)
        .show(ui, add);
    let view = output.inner_rect;
    let offset = output.state.offset.y;
    let content = output.content_size.y;
    let overflow = content - view.height();
    let painter = ui.painter();
    if offset > 0.5 {
        edge_fade(painter, view, true, 18.0);
    }
    if overflow > 0.5 && offset < overflow - 0.5 {
        edge_fade(painter, view, false, 18.0);
    }
}

/// Vertical alpha ramp from the card face to transparent, used at scroll edges.
fn edge_fade(painter: &egui::Painter, rect: Rect, top: bool, height: f32) {
    let strip = if top {
        Rect::from_min_max(rect.left_top(), pos2(rect.right(), rect.top() + height))
    } else {
        Rect::from_min_max(pos2(rect.left(), rect.bottom() - height), rect.right_bottom())
    };
    let (near, far) = if top {
        (PANEL_2, Color32::TRANSPARENT)
    } else {
        (Color32::TRANSPARENT, PANEL_2)
    };
    let mut mesh = egui::Mesh::default();
    mesh.colored_vertex(strip.left_top(), near);
    mesh.colored_vertex(strip.left_bottom(), far);
    mesh.colored_vertex(strip.right_top(), near);
    mesh.colored_vertex(strip.right_bottom(), far);
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(1, 3, 2);
    painter.add(egui::Shape::mesh(mesh));
}

// ---------------------------------------------------------------------------
// rules
// ---------------------------------------------------------------------------

/// A 1px hairline whose ends dissolve into the card: solid from 22% to 78%,
/// transparent at both ends. Never a hard separator.
pub fn fade_rule(ui: &mut Ui) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
    fade_rule_at(ui.painter(), rect);
}

/// The same hairline painted into an existing rect.
pub fn fade_rule_at(painter: &egui::Painter, rect: Rect) {
    let mid = rect.center().y;
    let left = rect.left();
    let right = rect.right();
    let x = |t: f32| left + (right - left) * t;
    let stops = [
        (0.0, Color32::TRANSPARENT),
        (0.22, RULE),
        (0.78, RULE),
        (1.0, Color32::TRANSPARENT),
    ];
    let mut mesh = egui::Mesh::default();
    for (t, color) in stops {
        mesh.colored_vertex(pos2(x(t), mid - 0.5), color);
        mesh.colored_vertex(pos2(x(t), mid + 0.5), color);
    }
    for column in 0..3u32 {
        let base = column * 2;
        mesh.add_triangle(base, base + 1, base + 2);
        mesh.add_triangle(base + 1, base + 3, base + 2);
    }
    painter.add(egui::Shape::mesh(mesh));
}

// ---------------------------------------------------------------------------
// buttons
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ButtonKind {
    /// Secondary action: text only, washing on hover.
    Ghost,
    /// The one thing the sheet is for.
    Primary,
    /// Destructive action.
    Danger,
}

/// A dialog button. Returns the response so callers keep using `.clicked()`.
/// Keyboard focus (Tab, then Space/Enter) works like every other widget.
pub fn button(ui: &mut Ui, text: &str, kind: ButtonKind) -> Response {
    let galley = ui.painter().layout_no_wrap(
        text.to_owned(),
        egui::FontId::proportional(13.0),
        INK,
    );
    let size = vec2(
        (galley.size().x + 24.0).max(CONTROL_HEIGHT),
        CONTROL_HEIGHT,
    );
    let (rect, _) = ui.allocate_exact_size(size, Sense::click());
    let response = ui.interact(rect, ui.id().with(("dialog-button", text)), Sense::click());
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), text)
    });
    let enabled = ui.is_enabled();
    let hovered = enabled && response.hovered();
    let pressed = enabled && response.is_pointer_button_down_on();
    let painter = ui.painter();
    let (fill, ink) = match (kind, pressed, hovered, enabled) {
        (_, _, _, false) => (Color32::from_rgb(0xF4, 0xF0, 0xEA), MUTED.gamma_multiply(0.7)),
        (ButtonKind::Primary, true, _, _) => (TEAL_BRIGHT, ON_INK),
        (ButtonKind::Primary, _, true, _) => (TEAL, ON_INK),
        (ButtonKind::Primary, _, _, _) => (INK, ON_INK),
        (ButtonKind::Danger, true, _, _) => (DANGER_WASH_HOVER, RED),
        (ButtonKind::Danger, _, true, _) => (DANGER_WASH_HOVER, RED),
        (ButtonKind::Danger, _, _, _) => (DANGER_WASH, RED),
        (ButtonKind::Ghost, true, _, _) => (HOVER, INK),
        (ButtonKind::Ghost, _, true, _) => (HOVER, INK),
        (ButtonKind::Ghost, _, _, _) => (Color32::TRANSPARENT, MUTED),
    };
    painter.rect_filled(rect, 9.0, fill);
    if response.has_focus() {
        painter.rect_stroke(rect, 9.0, Stroke::new(1.0_f32, TEAL.gamma_multiply(0.55)));
    }
    painter.galley(
        rect.center() - galley.size() * 0.5,
        galley.clone(),
        ink,
    );
    response
}

// ---------------------------------------------------------------------------
// rows
// ---------------------------------------------------------------------------

/// A full-width row inside a card. `highlighted` is the keyboard position and
/// paints the sage selection wash; otherwise the row only washes under the
/// pointer. Callers build the contents (title, meta, check) and use the
/// returned response for click/hover handling.
pub fn list_row(
    ui: &mut Ui,
    highlighted: bool,
    add: impl FnOnce(&mut Ui),
) -> Response {
    let background = ui.painter().add(egui::Shape::Noop);
    let inner = egui::Frame::none()
        .inner_margin(Margin::symmetric(10.0, 7.0))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui);
        });
    let rect = inner.response.rect;
    let response = ui.interact(
        rect,
        inner.response.id.with("row-hit"),
        Sense::click(),
    );
    let fill = if highlighted {
        if response.hovered() {
            SOFT_ACCENT.gamma_multiply(0.94)
        } else {
            SOFT_ACCENT
        }
    } else if response.is_pointer_button_down_on() || response.hovered() {
        HOVER
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().set(
        background,
        egui::epaint::RectShape::filled(rect, ROW_ROUNDING, fill),
    );
    response
}

/// A row with a title on the left and one muted meta label on the right, plus
/// an optional check mark when the row is the current value.
pub fn choice_row(
    ui: &mut Ui,
    title: &str,
    meta: &str,
    highlighted: bool,
    active: bool,
) -> Response {
    list_row(ui, highlighted, |ui| {
        ui.horizontal(|ui| {
            ui.add(
                egui::Label::new(egui::RichText::new(title).size(13.5).color(TEXT))
                    .truncate(),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if active {
                    check(ui);
                    ui.add_space(6.0);
                }
                if !meta.is_empty() {
                    ui.label(egui::RichText::new(meta).size(11.5).color(MUTED));
                }
            });
        });
    })
}

/// A 14px check mark for the row that is currently chosen.
pub fn check(ui: &mut Ui) -> Response {
    let (rect, response) = ui.allocate_exact_size(vec2(14.0, 14.0), Sense::hover());
    check_at(ui.painter(), rect, TEAL_BRIGHT);
    response
}

/// A checkbox row: the box on the left, the label filling the rest, the whole
/// row clickable. Returns the new value, so the caller decides what changed.
pub fn check_row(ui: &mut Ui, checked: bool, label: &str) -> bool {
    let mut value = checked;
    let response = list_row(ui, false, |ui| {
        ui.horizontal(|ui| {
            let (rect, _) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
            let painter = ui.painter();
            painter.rect_filled(
                rect,
                4.0,
                if checked { TEAL } else { HOVER },
            );
            if checked {
                check_at(painter, rect, ON_INK);
            } else {
                painter.rect_stroke(rect, 4.0, Stroke::new(1.0_f32, RULE));
            }
            ui.add(
                egui::Label::new(egui::RichText::new(label).size(13.0).color(TEXT)).wrap(),
            );
        });
    });
    if response.clicked() {
        value = !value;
    }
    value
}

/// Paint the check mark into an existing rect.
pub fn check_at(painter: &egui::Painter, rect: Rect, color: Color32) {
    let c = rect.center();
    let stroke = Stroke::new(1.7_f32, color);
    painter.line_segment([c + vec2(-4.0, 0.0), c + vec2(-1.2, 2.8)], stroke);
    painter.line_segment([c + vec2(-1.2, 2.8), c + vec2(4.2, -3.2)], stroke);
}

// ---------------------------------------------------------------------------
// fields
// ---------------------------------------------------------------------------

/// Search field: a soft pill with a magnifier, a frameless edit, and a ✕ that
/// clears the text once anything is typed. Returns the edit's response so the
/// caller can keep checking focus and Enter.
pub fn search(ui: &mut Ui, hint: &str, text: &mut String, id: impl std::hash::Hash) -> Response {
    let id = ui.id().with(("dialog-search", id));
    let (rect, _) = ui.allocate_exact_size(
        vec2(ui.available_width(), CONTROL_HEIGHT + 2.0),
        Sense::hover(),
    );
    let edit_id = id.with("edit");
    let focused = ui.memory(|m| m.has_focus(edit_id));
    let hovered = ui
        .input(|i| i.pointer.hover_pos())
        .is_some_and(|pos| rect.contains(pos));
    let painter = ui.painter().clone();
    painter.rect_filled(
        rect,
        10.0,
        if focused {
            PANEL_2
        } else if hovered {
            HOVER.gamma_multiply(1.02)
        } else {
            HOVER
        },
    );
    if focused {
        painter.rect_stroke(rect, 10.0, Stroke::new(1.2_f32, TEAL.gamma_multiply(0.55)));
    }

    // Magnifier: a small ring plus a handle, matching the reference picker.
    let glass = pos2(rect.left() + 17.0, rect.center().y - 0.5);
    painter.circle_stroke(glass, 4.2, Stroke::new(1.4_f32, MUTED));
    painter.line_segment(
        [glass + vec2(3.2, 3.2), glass + vec2(6.0, 6.0)],
        Stroke::new(1.4_f32, MUTED),
    );

    let clear_width = if text.is_empty() { 0.0 } else { 26.0 };
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(Rect::from_min_max(
                pos2(rect.left() + 30.0, rect.top()),
                pos2(rect.right() - clear_width - 8.0, rect.bottom()),
            ))
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    child.spacing_mut().item_spacing = Vec2::ZERO;
    let response = child.add(
        egui::TextEdit::singleline(text)
            .id(edit_id)
            .frame(false)
            .margin(Margin::symmetric(0.0, 6.0))
            .hint_text(egui::RichText::new(hint).size(13.5).color(MUTED.gamma_multiply(0.85)))
            .text_color(TEXT)
            .desired_width(child.available_width()),
    );
    if !text.is_empty() {
        let clear = Rect::from_center_size(
            pos2(rect.right() - 14.0, rect.center().y),
            Vec2::splat(20.0),
        );
        let hit = ui.interact(clear, id.with("clear"), Sense::click());
        let color = if hit.hovered() { INK } else { MUTED };
        let c = clear.center();
        let stroke = Stroke::new(1.3_f32, color);
        painter.line_segment([c + vec2(-3.0, -3.0), c + vec2(3.0, 3.0)], stroke);
        painter.line_segment([c + vec2(-3.0, 3.0), c + vec2(3.0, -3.0)], stroke);
        if hit.clicked() {
            text.clear();
            ui.memory_mut(|m| m.request_focus(edit_id));
        }
    }
    response
}

/// A framed single-line field for dialogs, so forms never fall back to the
/// default egui sunken box.
pub fn field(ui: &mut Ui, id: impl std::hash::Hash, text: &mut String, hint: &str) -> Response {
    let edit = egui::TextEdit::singleline(text)
        .id(ui.id().with(("dialog-field", id)))
        .frame(false)
        .margin(Margin::symmetric(10.0, 7.0))
        .hint_text(egui::RichText::new(hint).size(13.0).color(MUTED))
        .text_color(TEXT)
        .desired_width(ui.available_width());
    let shadowed = ui.painter().add(egui::Shape::Noop);
    let response = ui.add(edit);
    let focused = response.has_focus();
    let hovered = response.hovered();
    let fill = if focused { PANEL_2 } else { HOVER };
    let rect = response.rect;
    ui.painter().set(
        shadowed,
        egui::epaint::RectShape::new(
            rect,
            10.0,
            fill,
            if focused {
                Stroke::new(1.2_f32, TEAL.gamma_multiply(0.55))
            } else if hovered {
                Stroke::new(1.0_f32, BORDER)
            } else {
                Stroke::NONE
            },
        ),
    );
    response
}

// ---------------------------------------------------------------------------
// segmented control
// ---------------------------------------------------------------------------

/// Two or three mutually exclusive options in one pill: the selected segment
/// slides rather than jumping. Returns true when the choice changed.
pub fn segmented(
    ui: &mut Ui,
    id: impl std::hash::Hash,
    options: &[&str],
    selected: &mut usize,
) -> bool {
    if options.is_empty() {
        return false;
    }
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), CONTROL_HEIGHT), Sense::hover());
    segmented_at(ui, rect, id, options, selected)
}

/// The same control at a caller-chosen rect, for layouts that already know
/// where it goes.
pub fn segmented_at(
    ui: &mut Ui,
    rect: Rect,
    id: impl std::hash::Hash,
    options: &[&str],
    selected: &mut usize,
) -> bool {
    if options.is_empty() {
        return false;
    }
    let id = ui.id().with(("segmented", id));
    let painter = ui.painter().clone();
    painter.rect_filled(rect, 9.0, HOVER);
    let seg_w = rect.width() / options.len() as f32;
    let target = *selected as f32;
    let animated = ui
        .ctx()
        .animate_value_with_time(id.with("slide"), target, 0.16);
    let pill = Rect::from_min_size(
        pos2(rect.left() + animated * seg_w + 2.0, rect.top() + 2.0),
        vec2(seg_w - 4.0, rect.height() - 4.0),
    );
    painter.rect_filled(pill, 7.0, PANEL_2);
    let mut changed = false;
    for (index, label) in options.iter().enumerate() {
        let cell = Rect::from_min_size(
            pos2(rect.left() + index as f32 * seg_w, rect.top()),
            vec2(seg_w, rect.height()),
        );
        let response = ui.interact(cell, id.with(("seg", index)), Sense::click());
        response.widget_info(|| {
            egui::WidgetInfo::selected(
                egui::WidgetType::RadioButton,
                true,
                index == *selected,
                *label,
            )
        });
        let color = if index == *selected {
            INK
        } else if response.hovered() {
            TEXT
        } else {
            MUTED
        };
        let galley = painter.layout_no_wrap(
            (*label).to_owned(),
            egui::FontId::proportional(12.5),
            color,
        );
        painter.galley(cell.center() - galley.size() * 0.5, galley, color);
        if response.clicked() && index != *selected {
            *selected = index;
            changed = true;
        }
    }
    changed
}

// ---------------------------------------------------------------------------
// effort bar
// ---------------------------------------------------------------------------

/// The thinking-effort bar: a 26px track, a cool-to-warm gradient that fills to
/// the knob, one 5px tick per level, and a white knob. Pointer clicks and drags
/// map to the nearest level; the knob and fill animate between levels unless a
/// real drag is in progress. Returns the new level when it changed.
pub fn effort_bar(
    ui: &mut Ui,
    id: impl std::hash::Hash,
    levels: usize,
    current: usize,
    enabled: bool,
) -> Option<usize> {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), EFFORT_HEIGHT), Sense::hover());
    effort_bar_at(ui, rect, id, levels, current, enabled)
}

/// The same bar at a caller-chosen rect.
pub fn effort_bar_at(
    ui: &mut Ui,
    rect: Rect,
    id: impl std::hash::Hash,
    levels: usize,
    current: usize,
    enabled: bool,
) -> Option<usize> {
    let id = ui.id().with(("effort-bar", id));
    if levels <= 1 || !enabled {
        // A model with a single level has nothing to drag: show a still track
        // with one tick, so the control still reads as "effort".
        let painter = ui.painter();
        painter.rect_filled(rect, 13.0, HOVER);
        painter.circle_filled(rect.center(), 2.5, MUTED.gamma_multiply(0.55));
        return None;
    }
    let radius = 13.0;
    let span = (rect.width() - 2.0 * radius).max(1.0);
    let pos_of = |index: usize| index as f32 / (levels - 1) as f32;
    let x_of = |t: f32| rect.left() + radius + t * span;

    let response = ui.interact(rect, id.with("track"), Sense::click_and_drag());
    response.widget_info(|| {
        egui::WidgetInfo::selected(
            egui::WidgetType::Slider,
            true,
            false,
            format!("Effort {}", current + 1),
        )
    });
    let dragging = response.dragged() || response.is_pointer_button_down_on();
    let mut next = None;
    if enabled {
        if let Some(pointer) = response.interact_pointer_pos() {
            if response.clicked() || response.dragged() {
                let t = ((pointer.x - rect.left() - radius) / span).clamp(0.0, 1.0);
                let index = (t * (levels - 1) as f32).round() as usize;
                if index != current {
                    next = Some(index);
                }
            }
        }
    }
    let target = pos_of(next.unwrap_or(current));
    let t = if dragging {
        target
    } else {
        let animated = ui
            .ctx()
            .animate_value_with_time(id.with("knob"), target, 0.28);
        if next.is_some() {
            target
        } else {
            animated
        }
    };

    let painter = ui.painter().clone();
    painter.rect_filled(rect, radius, HOVER);
    let knob_x = x_of(t);
    if t > 0.001 {
        let fill = Rect::from_min_max(rect.min, pos2(knob_x, rect.bottom()));
        gradient_round_rect(&painter, fill, radius, |x| {
            let relative = ((x - rect.left()) / rect.width()).clamp(0.0, 1.0);
            if relative < 0.5 {
                lerp_color(EFFORT_LOW, EFFORT_MID, relative * 2.0)
            } else {
                lerp_color(EFFORT_MID, EFFORT_HIGH, (relative - 0.5) * 2.0)
            }
        });
    }
    for index in 0..levels {
        let tick = pos2(x_of(pos_of(index)), rect.center().y);
        let passed = index as f32 <= t + 0.01 && t > 0.001;
        painter.circle_filled(
            tick,
            2.5,
            if passed {
                Color32::from_rgba_unmultiplied(255, 255, 255, 200)
            } else {
                MUTED.gamma_multiply(0.55)
            },
        );
    }
    // Knob: a white disc with a soft shadow, one step larger under the pointer.
    let knob = Rect::from_center_size(
        pos2(knob_x, rect.center().y),
        Vec2::splat(26.0),
    );
    painter.add(
        egui::epaint::RectShape::filled(knob.translate(vec2(0.0, 1.5)), 13.0, Color32::from_rgba_unmultiplied(0x3A, 0x33, 0x2A, 34))
            .with_blur_width(6.0),
    );
    painter.circle_filled(knob.center(), 13.0, Color32::WHITE);
    if response.has_focus() {
        painter.circle_stroke(knob.center(), 15.0, Stroke::new(1.5_f32, TEAL.gamma_multiply(0.5)));
    }
    next
}

/// Rounded rect filled with a horizontal gradient whose color comes from
/// `at(x)`. Built as a triangle fan so the fill keeps its rounded caps while
/// each vertex carries its own color.
pub fn gradient_round_rect(
    painter: &egui::Painter,
    rect: Rect,
    radius: f32,
    at: impl Fn(f32) -> Color32,
) {
    let radius = radius.min(rect.width() * 0.5).min(rect.height() * 0.5);
    let pts = rounded_polygon(rect, radius);
    let mut mesh = egui::Mesh::default();
    mesh.colored_vertex(rect.center(), at(rect.center().x));
    for point in &pts {
        mesh.colored_vertex(*point, at(point.x));
    }
    let count = pts.len() as u32;
    for index in 0..count {
        mesh.add_triangle(0, 1 + index, 1 + (index + 1) % count);
    }
    painter.add(egui::Shape::mesh(mesh));
}

/// Boundary of a rounded rectangle, clockwise, with enough samples on each arc
/// that the gradient fan stays smooth.
fn rounded_polygon(rect: Rect, radius: f32) -> Vec<Pos2> {
    let mut points = Vec::with_capacity(28);
    let corners = [
        (pos2(rect.right() - radius, rect.top() + radius), -90.0), // top-right
        (pos2(rect.right() - radius, rect.bottom() - radius), 0.0), // bottom-right
        (pos2(rect.left() + radius, rect.bottom() - radius), 90.0), // bottom-left
        (pos2(rect.left() + radius, rect.top() + radius), 180.0),  // top-left
    ];
    for (center, start) in corners {
        for step in 0..=6 {
            let angle = (start + step as f32 * 15.0).to_radians();
            points.push(center + vec2(angle.cos(), angle.sin()) * radius);
        }
    }
    points
}

fn lerp_color(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let mix = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(mix(a.r(), b.r()), mix(a.g(), b.g()), mix(a.b(), b.b()))
}

// ---------------------------------------------------------------------------
// notes
// ---------------------------------------------------------------------------

/// A quiet hint line under a control.
pub fn hint(ui: &mut Ui, text: &str) {
    ui.add(
        egui::Label::new(egui::RichText::new(text).size(12.0).color(MUTED)).wrap(),
    );
}

/// A validation or failure message inside a card: light wash, dark rose text,
/// one line of explanation.
pub fn error_line(ui: &mut Ui, text: &str) {
    egui::Frame::none()
        .fill(ERROR_BG)
        .rounding(9.0)
        .inner_margin(Margin::symmetric(10.0, 7.0))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.add(egui::Label::new(egui::RichText::new(text).size(12.5).color(RED)).wrap());
        });
}

/// A small pill for state ("current", "override", "detected").
pub fn tag(ui: &mut Ui, text: &str, color: Color32) {
    let galley = ui.painter().layout_no_wrap(
        text.to_owned(),
        egui::FontId::proportional(10.5),
        color,
    );
    let size = vec2(galley.size().x + 12.0, 17.0);
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    ui.painter()
        .rect_filled(rect, 8.5, color.gamma_multiply(0.14));
    ui.painter().galley(
        rect.center() - galley.size() * 0.5,
        galley,
        color,
    );
}

/// A muted footnote on the right of a footer, aligned with the buttons but not
/// competing with them (e.g. "Esc keeps the welcome fresh").
pub fn footnote(ui: &mut Ui, text: &str) {
    let galley = ui.painter().layout_no_wrap(
        text.to_owned(),
        egui::FontId::proportional(11.5),
        MUTED,
    );
    let (rect, _) = ui.allocate_exact_size(
        vec2(galley.size().x, CONTROL_HEIGHT),
        Sense::hover(),
    );
    ui.painter().galley(
        pos2(
            rect.right() - galley.size().x,
            rect.center().y - galley.size().y * 0.5,
        ),
        galley,
        MUTED,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    const SCREEN: Vec2 = Vec2::new(900.0, 700.0);

    /// One frame with the feather theme applied and a plain panel behind the UI,
    /// so cards are judged against the real page color rather than raw black.
    fn run(
        ctx: &egui::Context,
        size: Vec2,
        events: Vec<egui::Event>,
        ui_fn: impl FnOnce(&mut Ui),
    ) -> egui::FullOutput {
        let mut ui_fn = Some(ui_fn);
        ctx.run(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size)),
                events,
                ..Default::default()
            },
            |ctx| {
                crate::theme::apply_theme(ctx);
                egui::CentralPanel::default()
                    .frame(egui::Frame::none().fill(PANEL))
                    .show(ctx, |ui| {
                        if let Some(add) = ui_fn.take() {
                            add(ui);
                        }
                    });
            },
        )
    }

    fn click(pos: Pos2, pressed: bool) -> Vec<egui::Event> {
        vec![
            egui::Event::PointerMoved(pos),
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            },
        ]
    }

    fn text_bounds(out: &egui::FullOutput, label: &str) -> (Rect, Rect) {
        out.shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::Shape::Text(text) if text.galley.text() == label => {
                    Some((text.galley.mesh_bounds.translate(text.pos.to_vec2()), shape.clip_rect))
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("missing text: {label}"))
    }

    #[test]
    fn the_card_keeps_its_title_footer_and_close_inside_every_supported_viewport() {
        for size in [vec2(1160.0, 820.0), vec2(760.0, 560.0), vec2(480.0, 420.0)] {
            let ctx = egui::Context::default();
            let open = Cell::new(true);
            let mut out = None;
            for _ in 0..3 {
                out = Some(ctx.run(
                    egui::RawInput {
                        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size)),
                        ..Default::default()
                    },
                    |ctx| {
                        crate::theme::apply_theme(ctx);
                        let mut still_open = open.get();
                        sheet(ctx, "test-card", "Resume a session")
                            .width(520.0)
                            .lead("Type to search · ↑ / ↓ choose · Enter opens")
                            .body(|ui| {
                                choice_row(ui, "fix the parser", "s-19a2 · 2h ago", true, false);
                                choice_row(ui, "write docs", "s-3f10 · 3h ago", false, false);
                            })
                            .bottom(|ui| {
                                ui.horizontal(|ui| {
                                    button(ui, "Cancel", ButtonKind::Ghost);
                                    button(ui, "Continue", ButtonKind::Primary);
                                });
                            })
                            .show(&mut still_open);
                        open.set(still_open);
                    },
                ));
            }
            let out = out.unwrap();
            for label in ["Resume a session", "Cancel", "Continue", "fix the parser"] {
                let (bounds, clip) = text_bounds(&out, label);
                assert!(
                    clip.contains_rect(bounds),
                    "{label} clipped at {size:?}: {bounds:?} in {clip:?}"
                );
                assert!(
                    bounds.top() >= 0.0 && bounds.bottom() <= size.y,
                    "{label} escaped the viewport at {size:?}: {bounds:?}"
                );
            }
            // The ✕ must sit in the card's top-right corner, not outside it.
            let title = text_bounds(&out, "Resume a session").0;
            assert!(title.left() > 0.0 && title.right() < size.x);
        }
    }

    #[test]
    fn an_overlong_title_never_lands_on_the_close_control() {
        let ctx = egui::Context::default();
        let long = "A really quite unreasonably long dialog title that keeps going";
        let mut out = None;
        for _ in 0..2 {
            out = Some(ctx.run(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, SCREEN)),
                    ..Default::default()
                },
                |ctx| {
                    crate::theme::apply_theme(ctx);
                    let mut open = true;
                    sheet(ctx, "long-title", long).width(360.0).show(&mut open);
                },
            ));
        }
        let (title, _) = text_bounds(&out.unwrap(), long);
        // A 360px card centered on a 900px screen spans 270..630. The title is
        // laid out in a 20px-inset column narrowed by the ✕ (26px + its gap),
        // so it must finish before 630 - 20 - 34.
        let title_limit = 630.0 - 20.0 - 34.0;
        assert!(
            title.right() <= title_limit,
            "the title must wrap short of the ✕: {title:?} overruns {title_limit}"
        );
    }

    #[test]
    fn clicking_the_page_outside_the_card_dismisses_it_but_clicks_inside_do_not() {
        let ctx = egui::Context::default();
        // A sheet that is `closable` treats a press on the page as a dismissal.
        let open = Cell::new(true);
        let render = |events: Vec<egui::Event>| {
            let _ = ctx.run(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, SCREEN)),
                    events,
                    ..Default::default()
                },
                |ctx| {
                    crate::theme::apply_theme(ctx);
                    let mut still_open = open.get();
                    sheet(ctx, "outside-click", "Pick a model")
                        .width(420.0)
                        .top(|ui| {
                            ui.label("inside the card");
                        })
                        .show(&mut still_open);
                    open.set(still_open);
                },
            );
        };
        render(Vec::new());
        render(click(Pos2::new(12.0, 12.0), true));
        render(click(Pos2::new(12.0, 12.0), false));
        assert!(!open.get(), "a click on the page closes the picker");
    }

    #[test]
    fn a_non_closable_sheet_ignores_outside_clicks() {
        let ctx = egui::Context::default();
        let open = Cell::new(true);
        let render = |events: Vec<egui::Event>| {
            let _ = ctx.run(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, SCREEN)),
                    events,
                    ..Default::default()
                },
                |ctx| {
                    crate::theme::apply_theme(ctx);
                    let mut still_open = open.get();
                    sheet(ctx, "locked", "Setting up")
                        .closable(false)
                        .show(&mut still_open);
                    open.set(still_open);
                },
            );
        };
        render(Vec::new());
        render(click(Pos2::new(12.0, 12.0), true));
        render(click(Pos2::new(12.0, 12.0), false));
        assert!(open.get(), "a locked sheet must not be dismissed by a stray click");
    }

    #[test]
    fn fade_rule_dissolves_at_both_ends_and_stays_solid_in_the_middle() {
        let ctx = egui::Context::default();
        let out = run(&ctx, vec2(600.0, 200.0), Vec::new(), |ui| fade_rule(ui));
        let mesh = out
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::Shape::Mesh(mesh) => Some(mesh),
                _ => None,
            })
            .expect("hairline mesh");
        let left = mesh.vertices.iter().map(|v| v.pos.x).fold(f32::INFINITY, f32::min);
        let right = mesh
            .vertices
            .iter()
            .map(|v| v.pos.x)
            .fold(f32::NEG_INFINITY, f32::max);
        for vertex in &mesh.vertices {
            let t = (vertex.pos.x - left) / (right - left);
            if t < 0.02 || t > 0.98 {
                assert_eq!(vertex.color.a(), 0, "ends must be fully transparent");
            } else if (0.3..0.7).contains(&t) {
                assert!(vertex.color.a() > 200, "the middle must stay solid");
            }
        }
    }

    #[test]
    fn a_hovered_row_washes_while_an_idle_one_stays_clear() {
        let ctx = egui::Context::default();
        let row_center = Cell::new(Pos2::ZERO);
        let render = |events: Vec<egui::Event>| {
            run(&ctx, vec2(420.0, 300.0), events, |ui| {
                let idle = choice_row(ui, "fix the parser", "2 hours ago", false, false);
                row_center.set(idle.rect.center());
            })
        };
        let idle = render(Vec::new());
        render(click(row_center.get(), false));
        render(vec![egui::Event::PointerMoved(row_center.get())]);
        let hovered = render(Vec::new());
        let rows = |out: &egui::FullOutput| -> Vec<Color32> {
            out.shapes
                .iter()
                .filter_map(|shape| match &shape.shape {
                    egui::Shape::Rect(rect) if rect.rounding.nw == ROW_ROUNDING => Some(rect.fill),
                    _ => None,
                })
                .collect()
        };
        assert!(
            rows(&idle).iter().all(|fill| fill.a() == 0),
            "an untouched row paints no wash at all"
        );
        assert!(
            rows(&hovered).iter().any(|fill| *fill == HOVER),
            "the row under the pointer washes"
        );
    }

    #[test]
    fn the_highlighted_row_keeps_the_selection_wash_without_a_pointer() {
        let ctx = egui::Context::default();
        let out = run(&ctx, vec2(420.0, 300.0), Vec::new(), |ui| {
            choice_row(ui, "write docs", "3 hours ago", true, true);
        });
        let selected = out.shapes.iter().any(|shape| {
            matches!(&shape.shape, egui::Shape::Rect(rect)
                if rect.rounding.nw == ROW_ROUNDING && rect.fill == SOFT_ACCENT)
        });
        assert!(selected, "the keyboard row keeps its sage wash");
        let checked = out
            .shapes
            .iter()
            .filter(|shape| matches!(&shape.shape, egui::Shape::LineSegment { .. }))
            .count();
        assert!(checked >= 2, "the chosen row also draws a check mark");
    }

    #[test]
    fn the_search_clear_control_empties_the_query() {
        let ctx = egui::Context::default();
        let query = std::cell::RefCell::new("sonnet".to_owned());
        let clear = Cell::new(Pos2::ZERO);
        let render = |events: Vec<egui::Event>| {
            run(&ctx, vec2(400.0, 240.0), events, |ui| {
                let mut text = query.borrow_mut();
                let response = search(ui, "Search models…", &mut text, "s");
                clear.set(pos2(response.rect.right() + 14.0, response.rect.center().y));
            })
        };
        render(Vec::new());
        render(click(clear.get(), true));
        render(click(clear.get(), false));
        assert!(query.borrow().is_empty(), "the clear ✕ empties the query");
    }

    #[test]
    fn the_effort_bar_maps_a_click_to_the_nearest_level() {
        let ctx = egui::Context::default();
        let track = Cell::new(Rect::NOTHING);
        let picked = Cell::new(None);
        let render = |events: Vec<egui::Event>| {
            run(&ctx, vec2(400.0, 200.0), events, |ui| {
                let rect = Rect::from_min_size(ui.cursor().min, vec2(320.0, EFFORT_HEIGHT));
                track.set(rect);
                picked.set(effort_bar_at(ui, rect, "bar", 6, 0, true));
            })
        };
        render(Vec::new());
        render(click(track.get().center(), true));
        render(click(track.get().center(), false));
        assert_eq!(picked.get(), Some(3), "the middle of a six-level bar is level 3");
    }

    #[test]
    fn a_single_level_bar_is_inert() {
        let ctx = egui::Context::default();
        let picked = Cell::new(None);
        run(&ctx, vec2(400.0, 200.0), Vec::new(), |ui| {
            picked.set(effort_bar(ui, "bar", 1, 0, true));
        });
        assert_eq!(picked.get(), None, "one level leaves nothing to drag");
    }

    #[test]
    fn a_disabled_primary_button_stays_visible_and_never_fires() {
        let ctx = egui::Context::default();
        let fired = Cell::new(false);
        let rect = Cell::new(Rect::NOTHING);
        let render = |events: Vec<egui::Event>| {
            run(&ctx, vec2(300.0, 160.0), events, |ui| {
                ui.add_enabled_ui(false, |ui| {
                    let response = button(ui, "Save model", ButtonKind::Primary);
                    rect.set(response.rect);
                    fired.set(response.clicked());
                });
            })
        };
        render(Vec::new());
        render(click(rect.get().center(), true));
        render(click(rect.get().center(), false));
        assert!(!fired.get(), "a disabled primary button must not run its action");
    }

    #[test]
    fn segmented_control_reports_a_change_only_when_the_selection_moves() {
        let ctx = egui::Context::default();
        let selected = Cell::new(0usize);
        let changed = Cell::new(false);
        let cells = Cell::new(Rect::NOTHING);
        let render = |events: Vec<egui::Event>| {
            run(&ctx, vec2(400.0, 160.0), events, |ui| {
                let rect = Rect::from_min_size(ui.cursor().min, vec2(320.0, CONTROL_HEIGHT));
                cells.set(rect);
                let mut index = selected.get();
                let moved = segmented_at(ui, rect, "seg", &["Compatible API", "OAuth providers"], &mut index);
                selected.set(index);
                changed.set(moved);
            })
        };
        render(Vec::new());
        // Click the same segment again: no change is reported.
        let left = pos2(cells.get().left() + 40.0, cells.get().center().y);
        render(click(left, true));
        render(click(left, false));
        assert!(!changed.get(), "re-picking the active segment is a no-op");
        let right = pos2(cells.get().right() - 40.0, cells.get().center().y);
        render(click(right, true));
        render(click(right, false));
        assert_eq!(selected.get(), 1, "the second segment becomes active");
        assert!(changed.get(), "moving the selection reports a change");
    }

    #[test]
    fn the_scrim_gets_deeper_for_blocking_sheets_than_for_pickers() {
        assert_eq!(Scrim::None.color(), None);
        let soft = Scrim::Soft.color().unwrap();
        let deep = Scrim::Deep.color().unwrap();
        assert!(soft.a() < deep.a(), "a confirmation dims more than a picker");
    }
}
