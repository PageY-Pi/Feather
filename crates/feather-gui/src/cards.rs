//! Soft conversation and tool-call surfaces. Pure egui, no web view.
#[path = "layout_cache.rs"]
mod layout_cache;
use crate::theme::*;
use crate::{ChatRow, RowKind};
use eframe::egui;

pub fn render_row(ui: &mut egui::Ui, row: &mut ChatRow, now: f64, show_thinking: bool) {
    match row.kind {
        RowKind::User => {
            let max = (ui.available_width() * 0.82).min(650.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                egui::Frame::none()
                    .fill(USER_BUBBLE)
                    .rounding(egui::Rounding {
                        nw: 20.0,
                        ne: 20.0,
                        sw: 20.0,
                        se: 7.0,
                    })
                    .inner_margin(egui::Margin::symmetric(18.0, 13.0))
                    .show(ui, |ui| {
                        ui.set_max_width(max);
                        // The bubble is right-anchored by its parent, but its
                        // contents are normal left-aligned paragraphs, not RTL.
                        ui.with_layout(egui::Layout::top_down(egui::Align::Min),|ui| {
                            ui.add(egui::Label::new(egui::RichText::new(&row.text).size(15.5).color(TEXT)).wrap());
                        });
                    });
            });
        }
        RowKind::Assistant => {
            ui.horizontal(|ui| {
                mascot(ui, 24.0);
                ui.label(
                    egui::RichText::new("Feather")
                        .size(12.5)
                        .strong()
                        .color(INK),
                );
                if row.streaming {
                    crate::activity::orbit(ui, 20.0, now);
                    ui.label(egui::RichText::new("writing…").size(12.0).color(MUTED));
                }
            });
            ui.add_space(1.0);
            egui::Frame::none()
                .fill(PANEL_2)
                .rounding(20.0)
                .inner_margin(egui::Margin::symmetric(20.0, 15.0))
                .show(ui, |ui| {
                    ui.set_width((ui.available_width() - 1.0).max(120.0));
                    if !row.thinking.is_empty() {
                        // Global toggle applies once per row, including virtualized
                        // rows revisited later. Header clicks remain independently usable.
                        let mode_id = ui.id().with("thinking-global-mode");
                        let previous = ui.data(|d|d.get_temp::<bool>(mode_id));
                        let force = (previous != Some(show_thinking)).then_some(show_thinking);
                        ui.data_mut(|d|d.insert_temp(mode_id,show_thinking));
                        egui::CollapsingHeader::new(
                            egui::RichText::new("Thoughts").size(12.5).color(TEAL),
                        )
                        .id_salt("thinking")
                        .default_open(true)
                        .open(force)
                        .show(ui, |ui| {
                            ui.spacing_mut().item_spacing.y = 0.0;
                            thinking_text(ui, &row.thinking, row.streaming);
                        });
                    }
                    if !row.text.is_empty() {
                        prose(ui, &row.text, row.streaming);
                    }
                    if row.streaming {
                        crate::activity::orbit(ui, 20.0, now);
                    }
                });
        }
        RowKind::Tool => render_tool(ui, row, now),
        RowKind::Notice => {
            ui.add(egui::Label::new(egui::RichText::new(&row.text).size(12.5).color(MUTED)).wrap());
        }
        RowKind::Error => {
            egui::Frame::none()
                .fill(crate::theme::ERROR_BG)
                .rounding(16.0)
                .inner_margin(egui::Margin::symmetric(16.0, 12.0))
                .show(ui, |ui| {
                    ui.label(
                        egui::RichText::new("A little snag")
                            .size(13.0)
                            .strong()
                            .color(RED),
                    );
                    ui.add(
                        egui::Label::new(egui::RichText::new(&row.text).size(14.0).color(TEXT))
                            .wrap(),
                    );
                });
        }
    }
}

fn render_tool(ui: &mut egui::Ui, row: &mut ChatRow, now: f64) {
    let tint = if row.tool_error {
        crate::theme::TOOL_ERROR_BG
    } else {
        TOOL_BG
    };
    let seconds = row
        .elapsed
        .or_else(|| row.started_at.map(|start| (now - start).max(0.0)));
    let status = if row.streaming {
        "Running"
    } else if row.tool_error {
        "Not found / error"
    } else {
        "Done"
    };
    let color = if row.tool_error { RED } else { TEAL };
    egui::Frame::none()
        .fill(tint)
        .rounding(16.0)
        .inner_margin(egui::Margin::symmetric(15.0, 11.0))
        .show(ui, |ui| {
            let width = ui.available_width();
            let header = ui
                .horizontal(|ui| {
                    let (rect, _) =
                        ui.allocate_exact_size(egui::vec2(22.0, 22.0), egui::Sense::hover());
                    ui.painter().rect_filled(rect, 7.0, PANEL_2);
                    ui.painter().text(
                        rect.center(),
                        egui::Align2::CENTER_CENTER,
                        if row.text == "bash" { ">_" } else { "{}" },
                        egui::FontId::monospace(11.0),
                        INK,
                    );
                    ui.label(
                        egui::RichText::new(&row.text)
                            .size(13.5)
                            .strong()
                            .color(TEXT),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let (rect, _) =
                            ui.allocate_exact_size(egui::vec2(14.0, 16.0), egui::Sense::hover());
                        let c = rect.center();
                        let dy = if row.expanded { -2.0 } else { 2.0 };
                        ui.painter().add(egui::Shape::line(
                            vec![
                                c + egui::vec2(-4.0, -dy),
                                c + egui::vec2(0.0, dy),
                                c + egui::vec2(4.0, -dy),
                            ],
                            egui::Stroke::new(1.4_f32, MUTED),
                        ));
                        ui.add_space(5.0);
                        if let Some(seconds) = seconds {
                            ui.label(
                                egui::RichText::new(format!("{seconds:.1}s"))
                                    .size(11.5)
                                    .color(MUTED),
                            );
                        }
                        if row.streaming {
                            crate::activity::orbit(ui, 18.0, now);
                        }
                        ui.label(egui::RichText::new(status).size(11.5).color(color));
                    });
                })
                .response;
            let hit = ui.interact(
                header.rect,
                ui.id().with("tool-header"),
                egui::Sense::click(),
            );
            if hit.clicked() {
                row.expanded = !row.expanded;
            }
            hit.on_hover_cursor(egui::CursorIcon::PointingHand);
            if !row.tool_args.is_empty() {
                // `Label::truncate()` already shows the full text as a tooltip
                // when the text elides (egui `label.rs`: `if galley.elided {
                // response.on_hover_text(galley.text()) }`). Adding our own
                // `.on_hover_text(&row.tool_args)` on top of it produced TWO
                // identical tooltips stacked vertically on any cut-off command.
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(&row.tool_args)
                            .monospace()
                            .size(12.0)
                            .color(MUTED),
                    )
                    .truncate(),
                );
            }
            if row.streaming && matches!(row.text.as_str(), "subagent" | "agent") {
                if let Some(progress) = &row.tool_output {
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(
                                progress.lines().take(4).collect::<Vec<_>>().join("\n"),
                            )
                            .size(12.0)
                            .color(TEAL),
                        )
                        .wrap(),
                    );
                }
            }
            if row.expanded {
                ui.add_space(5.0);
                egui::Frame::none()
                    .fill(PANEL_2)
                    .rounding(10.0)
                    .inner_margin(egui::Margin::same(11.0))
                    .show(ui, |ui| {
                        ui.set_width((width - 22.0).max(100.0));
                        egui::ScrollArea::vertical()
                            .id_salt("tool-output")
                            .max_height(230.0)
                            .auto_shrink([false, true])
                            .show(ui, |ui| {
                                let text = row
                                    .tool_output
                                    .as_deref()
                                    .unwrap_or("Waiting for the tool…");
                                ui.add(
                                    egui::Label::new(
                                        egui::RichText::new(text)
                                            .monospace()
                                            .size(13.0)
                                            .color(TEXT),
                                    )
                                    .wrap(),
                                );
                            });
                    });
            }
        });
}

/// Bytes of a live stream kept in the last, still-growing label.
///
/// Older bytes are sealed into stable chunks. The chunks are cut from the start of
/// the text and never re-cut from the moving end, so appending deltas does not
/// invalidate already-shaped galleys.
const LIVE_TAIL_BYTES: usize = 2_000;
const LIVE_BREAK_SEARCH: usize = 400;
#[cfg(test)]
const LIVE_CUT_QUANTUM: usize = 512;
const STABLE_CHUNK_TARGET: usize = 4 * 1024;

#[cfg(test)]
const LIVE_TAIL_MAX: usize = LIVE_TAIL_BYTES + LIVE_BREAK_SEARCH + LIVE_CUT_QUANTUM;

const LIVE_BREAKS: [char; 12] = [
    '\n', ' ', '\t', '。', '，', '！', '？', '；', '．', '.', ',', ';',
];

fn floor_boundary(text: &str, mut index: usize) -> usize {
    while index > 0 && !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

fn ceil_boundary(text: &str, mut index: usize) -> usize {
    while index < text.len() && !text.is_char_boundary(index) {
        index += 1;
    }
    index
}

fn natural_cut_after(text: &str, low: usize, high: usize) -> Option<usize> {
    let low = floor_boundary(text, low.min(text.len()));
    let high = ceil_boundary(text, high.min(text.len()));
    if low >= high {
        return None;
    }
    let window = &text[low..high];
    ['\n', ' ']
        .iter()
        .find_map(|brk| window.rfind(*brk))
        .or_else(|| window.rfind(LIVE_BREAKS))
        .map(|i| low + i + text[low + i..].chars().next().map_or(1, char::len_utf8))
        .filter(|at| *at > 0 && *at < text.len())
}

fn next_stable_chunk_end(text: &str, start: usize, seal_limit: usize) -> Option<usize> {
    if seal_limit.saturating_sub(start) < STABLE_CHUNK_TARGET {
        return None;
    }
    let target = start + STABLE_CHUNK_TARGET;
    let low = target.saturating_sub(LIVE_BREAK_SEARCH).max(start + 1);
    let high = target;
    let at = natural_cut_after(text, low, high).unwrap_or_else(|| floor_boundary(text, target));
    (at > start && at <= seal_limit).then_some(at)
}

/// Split a still-growing stream into stable chunk ranges plus a bounded live tail.
#[cfg(test)]
fn live_ranges(
    text: &str,
    streaming: bool,
) -> (Vec<std::ops::Range<usize>>, std::ops::Range<usize>) {
    if text.len() <= LIVE_TAIL_BYTES + LIVE_BREAK_SEARCH + LIVE_CUT_QUANTUM {
        return (Vec::new(), 0..text.len());
    }
    let seal_limit = if streaming {
        floor_boundary(
            text,
            (text.len() - LIVE_TAIL_BYTES) / LIVE_CUT_QUANTUM * LIVE_CUT_QUANTUM,
        )
    } else {
        text.len()
    };
    let mut ranges = Vec::new();
    let mut start = 0;
    while let Some(end) = next_stable_chunk_end(text, start, seal_limit) {
        ranges.push(start..end);
        start = end;
    }
    if start == 0 {
        (Vec::new(), 0..text.len())
    } else {
        (ranges, start..text.len())
    }
}

#[cfg(test)]
fn live_parts(text: &str, streaming: bool) -> (Option<&str>, &str) {
    let (ranges, tail) = live_ranges(text, streaming);
    if ranges.is_empty() {
        (None, text)
    } else {
        (Some(&text[..tail.start]), &text[tail])
    }
}

#[derive(Clone, Copy, Default)]
struct ScrollPass {
    anchor_top: Option<f32>,
    freeze_geometry: bool,
    delta: f32,
    total_shift: f32,
}

pub fn begin_row_scroll(ctx: &egui::Context, anchor_top: Option<f32>, freeze_geometry: bool) {
    ctx.data_mut(|d| {
        d.insert_temp(
            egui::Id::new("feather-row-scroll-pass"),
            ScrollPass {
                anchor_top,
                freeze_geometry,
                delta: 0.0,
                total_shift: 0.0,
            },
        )
    });
}

pub fn release_history_layouts(ctx:&egui::Context) {
    *layout_cache::galley_ring(ctx).lock().unwrap()=layout_cache::GalleyRing::default();
}
#[cfg(test)]
pub fn clear_layout_ring_for_test(ctx: &egui::Context) {
    *layout_cache::galley_ring(ctx).lock().unwrap() = layout_cache::GalleyRing::default();
}

pub fn end_row_scroll(ctx: &egui::Context) -> f32 {
    ctx.data_mut(|d| d.remove_temp::<ScrollPass>(egui::Id::new("feather-row-scroll-pass")))
        .map_or(0.0, |s| s.delta)
}

fn fingerprint(text: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut h);
    h.finish()
}
struct CachedPiece {
    start: usize,
    end: usize,
    hash: u64,
    width: u32,
    ppp: u32,
    atlas: usize,
    estimated_height: f32,
    measured_height: Option<f32>,
    galley: Option<std::sync::Weak<egui::Galley>>,
}

#[derive(Default)]
struct StreamCache {
    // A weak guard prevents allocator address reuse without retaining old atlas pixels.
    atlas_guard: Option<std::sync::Weak<egui::epaint::mutex::Mutex<egui::epaint::TextureAtlas>>>,
    pieces: Vec<CachedPiece>,
    ends: Vec<usize>,
    source_ptr: usize,
    source_len: usize,
}

#[cfg(test)]
std::thread_local! {static LAYOUT_CALLS: std::cell::Cell<usize> = const {std::cell::Cell::new(0)};}

fn estimate_text_height(text: &str, width: f32, font_id: &egui::FontId) -> f32 {
    let line_height = (font_id.size * 1.35).max(1.0);
    let avg_char_width = (font_id.size * 0.62).max(1.0);
    let chars_per_line = (width / avg_char_width).max(1.0);
    let mut visual_lines = 0.0f32;
    for line in text.split('\n') {
        visual_lines += ((line.chars().count() as f32) / chars_per_line)
            .ceil()
            .max(1.0);
    }
    visual_lines * line_height
}
fn layout_cached_piece(
    ui: &egui::Ui,
    piece: &str,
    width: f32,
    font_id: egui::FontId,
    color: egui::Color32,
    width_key: u32,
    ppp_key: u32,
    atlas_key: usize,
) -> std::sync::Arc<egui::Galley> {
    let key = layout_cache::GalleyKey {
        hash: fingerprint(piece),
        len: piece.len(),
        width: width_key,
        ppp: ppp_key,
        atlas: atlas_key,
        mono: matches!(font_id.family, egui::FontFamily::Monospace),
        size_bits: font_id.size.to_bits(),
        color: color.to_array(),
    };
    let ring = layout_cache::galley_ring(ui.ctx());
    ring.lock()
        .expect("galley ring poisoned")
        .bind_atlas(&ui.fonts(|f| f.texture_atlas()));
    if let Some(galley) = ring.lock().expect("galley ring poisoned").get(&key) {
        return galley;
    }
    #[cfg(test)]
    LAYOUT_CALLS.with(|calls| calls.set(calls.get() + 1));
    let galley = ui.fonts(|fonts| fonts.layout(piece.to_owned(), font_id, color, width));
    let cached = ring
        .lock()
        .expect("galley ring poisoned")
        .insert(key, galley);
    cached
}

fn show_piece_lazy(
    ui: &mut egui::Ui,
    piece: &mut CachedPiece,
    text: &str,
    width: f32,
    font_id: egui::FontId,
    color: egui::Color32,
    width_key: u32,
    ppp_key: u32,
    atlas_key: usize,
    shape_budget: &mut usize,
) {
    let piece_text = &text[piece.start..piece.end];
    if piece.width != width_key || piece.ppp != ppp_key {
        piece.estimated_height = estimate_text_height(piece_text, width, &font_id);
        piece.measured_height = None;
        piece.galley = None;
    } else if piece.atlas != atlas_key {
        // UV invalidation is not geometry invalidation.
        piece.galley = None;
    }
    piece.width = width_key;
    piece.ppp = ppp_key;
    piece.atlas = atlas_key;
    let pass_id = egui::Id::new("feather-row-scroll-pass");
    let pass = ui
        .ctx()
        .data(|d| d.get_temp::<ScrollPass>(pass_id))
        .unwrap_or_default();
    let pos = ui.cursor().min;
    let old_height = piece.estimated_height;
    let old_bottom = pos.y - pass.total_shift + old_height;
    let probe = egui::Rect::from_min_size(pos, egui::vec2(width, old_height));
    let visible = probe.intersects(ui.clip_rect().expand(1200.0));
    let mut galley = piece.galley.as_ref().and_then(std::sync::Weak::upgrade);
    if visible && galley.is_none() {
        if *shape_budget > 0 {
            let shaped = layout_cached_piece(
                ui, piece_text, width, font_id, color, width_key, ppp_key, atlas_key,
            );
            piece.measured_height = Some(shaped.size().y);
            piece.galley = Some(std::sync::Arc::downgrade(&shaped));
            galley = Some(shaped);
            *shape_budget -= 1;
        } else {
            ui.ctx().request_repaint_after(crate::graphics::SHAPING_REPAINT_DELAY);
        }
    }
    let draw_height = if pass.freeze_geometry {
        old_height
    } else {
        piece.measured_height.unwrap_or(old_height)
    };
    let delta = draw_height - old_height;
    piece.estimated_height = draw_height;
    if delta.abs() > 0.01 {
        ui.ctx().data_mut(|d| {
            if let Some(mut pass) = d.get_temp::<ScrollPass>(pass_id) {
                if pass.anchor_top.is_some_and(|top| old_bottom <= top) {
                    pass.delta += delta;
                }
                pass.total_shift += delta;
                d.insert_temp(pass_id, pass);
            }
        });
    }
    let size = egui::vec2(width, draw_height);
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    if rect.intersects(ui.clip_rect()) {
        if let Some(galley) = galley {
            ui.painter()
                .with_clip_rect(ui.clip_rect().intersect(rect))
                .galley(rect.min, galley, color);
        }
    }
}

/// Draw streamed text as immutable measured chunks plus a bounded live tail.
fn streamed_text(
    ui: &mut egui::Ui,
    cache_id: egui::Id,
    text: &str,
    streaming: bool,
    font_id: egui::FontId,
    color: egui::Color32,
) {
    let width = ui.available_width().max(1.0);
    let width_key = width.round() as u32;
    let ppp_key = (ui.ctx().pixels_per_point() * 1024.0).round() as u32;
    let atlas = ui.fonts(|fonts| fonts.texture_atlas());
    let atlas_key = std::sync::Arc::as_ptr(&atlas) as usize;

    let source_ptr = text.as_ptr() as usize;
    let source_len = text.len();
    let cache_cell = ui
        .data_mut(|d| d.remove_temp::<std::sync::Arc<std::sync::Mutex<StreamCache>>>(cache_id))
        .unwrap_or_else(|| std::sync::Arc::new(std::sync::Mutex::new(StreamCache::default())));
    let mut cache = cache_cell.lock().expect("stream text cache poisoned");
    if source_len < cache.source_len
        || (cache.source_ptr != 0 && cache.source_ptr != source_ptr && {
            let mut start = 0;
            cache.pieces.iter().zip(&cache.ends).any(|(piece, end)| {
                let matches = text.get(start..*end).is_some_and(|s| {
                    fingerprint(s) == piece.hash && piece.start == start && piece.end == *end
                });
                start = *end;
                !matches
            })
        })
    {
        *cache = StreamCache::default();
    }
    cache.atlas_guard = Some(std::sync::Arc::downgrade(&atlas));
    cache.source_ptr = source_ptr;
    cache.source_len = source_len;

    let seal_limit = if streaming {
        source_len.saturating_sub(LIVE_TAIL_BYTES)
    } else {
        source_len
    };
    let mut start = cache.ends.last().copied().unwrap_or(0);
    while let Some(end) = next_stable_chunk_end(text, start, seal_limit) {
        let piece = &text[start..end];
        cache.pieces.push(CachedPiece {
            start,
            end,
            hash: fingerprint(piece),
            width: width_key,
            ppp: ppp_key,
            atlas: atlas_key,
            estimated_height: estimate_text_height(piece, width, &font_id),
            measured_height: None,
            galley: None,
        });
        cache.ends.push(end);
        start = end;
    }

    let mut shape_budget = 8usize;
    for piece in &mut cache.pieces {
        show_piece_lazy(
            ui,
            piece,
            text,
            width,
            font_id.clone(),
            color,
            width_key,
            ppp_key,
            atlas_key,
            &mut shape_budget,
        );
    }
    drop(cache);
    ui.data_mut(|d| d.insert_temp(cache_id, cache_cell));

    let tail_text = &text[start..];
    if !tail_text.is_empty() {
        let galley = layout_cached_piece(
            ui, tail_text, width, font_id, color, width_key, ppp_key, atlas_key,
        );
        let size = galley.size();
        let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
        if rect.intersects(ui.clip_rect()) {
            ui.painter().galley(rect.min, galley, color);
        }
    }
}

/// The model's reasoning, dimmed and scrollable-by-nature; drawn in the pieces
/// [`live_parts`] chooses so a fast stream never reshapes the whole block.
fn thinking_text(ui: &mut egui::Ui, text: &str, streaming: bool) {
    streamed_text(
        ui,
        ui.id().with("thinking-text"),
        text,
        streaming,
        egui::FontId::proportional(13.5),
        MUTED,
    );
}

/// A small streaming-friendly presentation layer for the common coding-agent
/// prose structures. Raw/incomplete Markdown always remains visible.
fn prose(ui: &mut egui::Ui, text: &str, streaming: bool) {
    let mut code = false;
    let mut code_index = 0usize;
    let mut buffer = String::new();
    for line in text.lines() {
        if line.starts_with("```") {
            if code {
                code_block(ui, code_index, &buffer, streaming);
                code_index += 1;
                buffer.clear();
            }
            code = !code;
            continue;
        }
        if code {
            buffer.push_str(line);
            buffer.push('\n');
            continue;
        }
        if line.is_empty() {
            ui.add_space(6.0);
            continue;
        }
        let headings = line.chars().take_while(|c| *c == '#').count();
        if (1..=4).contains(&headings) && line.as_bytes().get(headings) == Some(&b' ') {
            ui.label(
                egui::RichText::new(&line[headings + 1..])
                    .size(if headings <= 2 { 20.0 } else { 17.0 })
                    .strong()
                    .color(INK),
            );
        } else if line.starts_with("- ") || line.starts_with("* ") {
            ui.horizontal_top(|ui| {
                ui.label(egui::RichText::new("•").color(TEAL));
                ui.add(egui::Label::new(inline(&line[2..])).wrap());
            });
        } else {
            ui.add(egui::Label::new(inline(line)).wrap());
        }
    }
    if !buffer.is_empty() {
        code_block(ui, code_index, &buffer, streaming);
    }
}

fn code_block(ui: &mut egui::Ui, index: usize, text: &str, streaming: bool) {
    egui::Frame::none()
        .fill(TOOL_BG)
        .rounding(12.0)
        .inner_margin(egui::Margin::same(12.0))
        .show(ui, |ui| {
            // A fenced block is streamed line by line while the closing fence is
            // still missing, so it grows inside one label — same reshape problem
            // as the thinking block, same split.
            ui.spacing_mut().item_spacing.y = 0.0;
            streamed_text(
                ui,
                ui.id().with(("code-block", index)),
                text.trim_end(),
                streaming,
                egui::FontId::monospace(13.0),
                TEXT,
            );
        });
}

fn inline(text: &str) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    let mut code = false;
    let mut bold = false;
    let mut rest = text;
    while !rest.is_empty() {
        if let Some(tail) = rest.strip_prefix('`') {
            code = !code;
            rest = tail;
            continue;
        }
        if let Some(tail) = rest.strip_prefix("**") {
            bold = !bold;
            rest = tail;
            continue;
        }
        let end = rest.find(['`', '*']).filter(|i| *i > 0).unwrap_or_else(|| {
            if rest.starts_with('*') {
                1
            } else {
                rest.len()
            }
        });
        let format = egui::TextFormat {
            font_id: if code {
                egui::FontId::monospace(13.5)
            } else {
                egui::FontId::proportional(15.5)
            },
            color: if bold { INK } else { TEXT },
            background: if code {
                TOOL_BG
            } else {
                egui::Color32::TRANSPARENT
            },
            extra_letter_spacing: if bold { 0.1 } else { 0.0 },
            ..Default::default()
        };
        job.append(&rest[..end], 0.0, format);
        rest = &rest[end..];
    }
    job
}

#[cfg(all(test, target_os = "windows"))]
mod tests {
    #[test]
    fn user_bubble_is_right_anchored_but_each_line_is_left_aligned() {
        for width in [360.0,900.0] {
            let ctx=egui::Context::default();crate::theme::apply_theme(&ctx);
            let text="A longer first line of a prompt\nshort\nA wrapping paragraph of uneven words. ".repeat(6);
            let mut row=ChatRow::new(RowKind::User,text);
            let mut output=None;
            for _ in 0..3 {
                output=Some(ctx.run(egui::RawInput{screen_rect:Some(egui::Rect::from_min_size(egui::Pos2::ZERO,egui::vec2(width,800.0))),..Default::default()},|ctx| {
                    egui::CentralPanel::default().show(ctx,|ui|render_row(ui,&mut row,0.0,true));
                }));
            }
            let output=output.unwrap();
            let bubble=output.shapes.iter().find_map(|s|match &s.shape {egui::Shape::Rect(r) if r.fill==USER_BUBBLE=>Some(r.rect),_=>None}).unwrap();
            assert!((bubble.right()-(width-8.0)).abs()<1.0,"bubble not on right: {bubble:?}");
            assert!(bubble.width()<=650.0+36.0+1.0);
            let text=output.shapes.iter().find_map(|s|match &s.shape{egui::Shape::Text(t)=>Some(t),_=>None}).unwrap();
            assert_eq!(text.galley.job.halign,egui::Align::Min);
            assert!(text.galley.rows.len()>4);
            for line in &text.galley.rows {assert!((line.rect.left()).abs()<0.1,"line must begin at the left edge: {:?}",line.rect);}
            assert!((text.pos.x-bubble.left()-18.0).abs()<1.0);
        }
    }
    use super::*;
    #[test]
    fn global_thinking_toggle_folds_header_without_hiding_it() {
        let ctx=egui::Context::default();ctx.style_mut(|s|s.animation_time=0.0);
        let mut row=ChatRow::new(RowKind::Assistant,"answer");row.thinking="unique reasoning body".into();
        let mut heights=Vec::new();
        for expanded in [true,false,true] {
            let output=ctx.run(egui::RawInput {screen_rect:Some(egui::Rect::from_min_size(egui::Pos2::ZERO,egui::vec2(800.0,600.0))),..Default::default()},|ctx| {
                egui::CentralPanel::default().show(ctx,|ui| {
                    let start=ui.cursor().min.y;render_row(ui,&mut row,0.0,expanded);heights.push(ui.cursor().min.y-start);
                });
            });
            let texts:Vec<&str>=output.shapes.iter().filter_map(|s|match &s.shape {egui::Shape::Text(t)=>Some(t.galley.job.text.as_str()),_=>None}).collect();
            assert!(texts.contains(&"Thoughts"),"collapsed thinking header must remain visible");
            assert_eq!(texts.iter().any(|s|s.contains("unique reasoning body")),expanded);
        }
        assert!(heights[0]>heights[1]);assert!(heights[2]>heights[1]);
    }
    #[test]
    fn expanded_tool_card_contains_chinese_argument_and_output_galleys() {
        let ctx = egui::Context::default();
        install_fonts(&ctx);
        let mut row = ChatRow::new(RowKind::Tool, "bash");
        row.expanded = true;
        row.tool_args = "printf '中文工具输出'".into();
        row.tool_output = Some("中文工具输出：羽毛\n错误路径：中文测试\n[exit 0]".into());
        let output = ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(900.0, 600.0),
                )),
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| render_row(ui, &mut row, 1.0, false));
            },
        );
        let text: Vec<_> = output
            .shapes
            .iter()
            .filter_map(|s| {
                if let egui::Shape::Text(t) = &s.shape {
                    Some(t)
                } else {
                    None
                }
            })
            .collect();
        for sample in ["中文工具输出", "错误路径：中文测试"] {
            let shape = text
                .iter()
                .find(|t| t.galley.text().contains(sample))
                .expect("Chinese card text must be laid out");
            assert!(shape.galley.num_vertices > 0);
        }
        assert!(!row.tool_output.as_ref().unwrap().contains('\u{fffd}'));
    }

    #[test]
    fn live_stream_is_split_into_settled_text_plus_tail() {
        let cjk = "先看目录结构，再决定改哪里。".repeat(400);
        let (settled, tail) = live_parts(&cjk, true);
        let settled = settled.expect("a long live stream must be split");
        assert_eq!(format!("{settled}{tail}"), cjk, "no character may be lost");
        assert!(
            tail.len() < settled.len(),
            "the live tail must be the small part"
        );

        let prose = "Reading the parser first.\nAnd the tokenizer.\n".repeat(200);
        let (settled, tail) = live_parts(&prose, true);
        assert!(settled.is_some(), "multi-line streams split too");
        assert_eq!(format!("{}{tail}", settled.unwrap()), prose);
        let settled = settled.unwrap();
        assert!(
            LIVE_BREAKS.contains(&settled.chars().next_back().unwrap()),
            "the seam prefers a break character when one is nearby: {:?}",
            &settled[settled.len().saturating_sub(40)..]
        );

        let code = format!(
            "```\n{}```\n",
            "    let value = compute(argument);\n".repeat(400)
        );
        let (settled, tail) = live_parts(&code, true);
        assert_eq!(format!("{}{tail}", settled.unwrap()), code);

        // Long completed text is chunked too, so finalized thoughts are never one
        // enormous label; short streams keep the single-label path.
        let (settled, tail) = live_parts(&cjk, false);
        assert_eq!(format!("{}{tail}", settled.unwrap()), cjk);
        assert_eq!(live_parts("short", true), (None, "short"));
    }

    #[test]
    fn a_growing_code_block_is_drawn_in_cached_and_live_pieces() {
        let ctx = egui::Context::default();
        install_fonts(&ctx);
        let code = "    let value = compute(argument);\n".repeat(300);
        let mut row = ChatRow::new(RowKind::Assistant, "");
        row.streaming = true;
        row.text = format!("Here is the change:\n\n```rust\n{code}```\n");
        let output = ctx.run(
            egui::RawInput {
                // Tall enough that nothing is culled: culled shapes never reach
                // `output.shapes`, and this test asserts on painting.
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(900.0, 6_000.0),
                )),
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| render_row(ui, &mut row, 1.0, false));
            },
        );
        let pieces: Vec<String> = output
            .shapes
            .iter()
            .filter_map(|shape| match &shape.shape {
                egui::Shape::Text(text) if code.contains(text.galley.text()) => {
                    (!text.galley.text().is_empty()).then(|| text.galley.text().to_string())
                }
                _ => None,
            })
            .collect();
        assert!(
            pieces.len() >= 2,
            "a live code block must not be one growing label: {pieces:?}"
        );
        assert_eq!(
            pieces.concat(),
            code.trim_end(),
            "the settled and live pieces must spell out the same code"
        );
        assert!(
            pieces.last().unwrap().len() <= LIVE_TAIL_MAX,
            "only the small live tail is reshaped per repaint"
        );
    }

    #[test]
    fn prefix_chunks_stay_stable_when_deltas_append() {
        let base = "段落一：先稳定前缀，再追加尾部。\n".repeat(900);
        let appended = format!("{base}{}", "新增 delta。".repeat(80));
        let (base_ranges, _) = live_ranges(&base, true);
        let (next_ranges, _) = live_ranges(&appended, true);
        assert!(!base_ranges.is_empty());
        assert!(next_ranges.len() >= base_ranges.len());
        for (before, after) in base_ranges.iter().zip(&next_ranges) {
            assert_eq!(before, after, "completed chunk ranges must not slide");
            assert_eq!(&base[before.clone()], &appended[after.clone()]);
        }
    }

    #[test]
    fn huge_cjk_thought_paints_only_visible_vertices_and_repaint_is_stable() {
        let ctx = egui::Context::default();
        install_fonts(&ctx);
        let mut row = ChatRow::new(RowKind::Assistant, "done");
        row.streaming = true;
        row.thinking = "这是一个很长的推理片段，用来验证视口裁剪不会提交隐藏文字。".repeat(4_000);

        let run = |ctx: &egui::Context, row: &mut ChatRow, time: f64| {
            ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(760.0, 360.0),
                    )),
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| render_row(ui, row, time, true));
                },
            )
        };

        let first = run(&ctx, &mut row, 1.0);
        let second = run(&ctx, &mut row, 1.5);
        let vertices = |out: &egui::FullOutput| -> usize {
            out.shapes
                .iter()
                .filter_map(|shape| match &shape.shape {
                    egui::Shape::Text(text) => Some(text.galley.num_vertices),
                    _ => None,
                })
                .sum()
        };
        assert!(
            vertices(&first) < 20_000,
            "hidden 100k+ CJK text must not be tessellated"
        );
        assert_eq!(
            vertices(&first),
            vertices(&second),
            "caret/orbit repaint must keep text bounds"
        );
    }

    #[test]
    fn first_frame_lazy_layout_is_bounded_for_huge_distinct_cjk() {
        LAYOUT_CALLS.with(|calls| calls.set(0));
        let ctx = egui::Context::default();
        install_fonts(&ctx);
        let mut row = ChatRow::new(RowKind::Assistant, "done");
        row.streaming = false;
        let distinct: String = (0..140_000)
            .map(|i| char::from_u32(0x4e00 + (i % 2_000) as u32).unwrap())
            .collect();
        row.thinking = distinct;

        let output = ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(760.0, 360.0),
                )),
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| render_row(ui, &mut row, 1.0, true));
            },
        );
        let layout_calls = LAYOUT_CALLS.with(|calls| calls.get());
        let text_shapes = output
            .shapes
            .iter()
            .filter(|shape| matches!(shape.shape, egui::Shape::Text(_)))
            .count();
        let vertices: usize = output
            .shapes
            .iter()
            .filter_map(|shape| match &shape.shape {
                egui::Shape::Text(text) => Some(text.galley.num_vertices),
                _ => None,
            })
            .sum();
        assert!(
            layout_calls <= 8,
            "first frame must not shape every hidden chunk: {layout_calls}"
        );
        assert!(
            text_shapes <= 10,
            "culled chunks must not submit text shapes: {text_shapes}"
        );
        assert!(
            vertices < 20_000,
            "visible vertices must stay viewport-bounded: {vertices}"
        );
        assert_eq!(
            row.thinking.chars().count(),
            140_000,
            "data is not truncated"
        );
    }

    #[test]
    fn global_galley_ring_is_bounded() {
        let ctx = egui::Context::default();
        install_fonts(&ctx);
        for i in 0..180 {
            let text = format!("chunk {i}: {}", "缓存边界测试".repeat(300));
            let _ = ctx.run(Default::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let _ = layout_cached_piece(
                        ui,
                        &text,
                        500.0,
                        egui::FontId::proportional(13.5),
                        TEXT,
                        500,
                        (ctx.pixels_per_point() * 1024.0) as u32,
                        ui.fonts(|fonts| std::sync::Arc::as_ptr(&fonts.texture_atlas()) as usize),
                    );
                });
            });
        }
        let (entries, bytes) = layout_cache::stats(&ctx);
        assert!(entries <= layout_cache::MAX_ENTRIES, "entries={entries}");
        assert!(bytes <= layout_cache::MAX_BYTES, "bytes={bytes}");
    }

    #[test]
    fn frozen_piece_geometry_commits_once_and_survives_atlas_eviction() {
        let ctx = egui::Context::default();
        install_fonts(&ctx);
        let text = "中文推理".repeat(300);
        let mut piece = CachedPiece {
            start: 0,
            end: text.len(),
            hash: fingerprint(&text),
            width: 500,
            ppp: 1024,
            atlas: 0,
            estimated_height: 900.0,
            measured_height: None,
            galley: None,
        };
        let mut delta = 0.0;
        let mut run = |freeze: bool, piece: &mut CachedPiece| {
            let _ = ctx.run(Default::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    begin_row_scroll(ctx, Some(10000.0), freeze);
                    let mut budget = 8;
                    let atlas = ui.fonts(|f| std::sync::Arc::as_ptr(&f.texture_atlas()) as usize);
                    show_piece_lazy(
                        ui,
                        piece,
                        &text,
                        500.0,
                        egui::FontId::proportional(13.5),
                        TEXT,
                        500,
                        1024,
                        atlas,
                        &mut budget,
                    );
                    delta = end_row_scroll(ctx);
                });
            });
            delta
        };
        for _ in 0..3 {
            assert_eq!(run(true, &mut piece), 0.0);
            assert_eq!(piece.estimated_height, 900.0);
        }
        let actual = piece.measured_height.unwrap();
        assert!((actual - 900.0).abs() > 1.0);
        assert!((run(false, &mut piece) - (actual - 900.0)).abs() < 0.01);
        assert_eq!(run(false, &mut piece), 0.0);
        clear_layout_ring_for_test(&ctx);
        piece.atlas = 0;
        assert_eq!(run(false, &mut piece), 0.0);
        assert_eq!(piece.estimated_height, actual);
    }

    #[test]
    fn scroll_correction_reports_measured_height_above_anchor() {
        let ctx = egui::Context::default();
        install_fonts(&ctx);
        let mut row = ChatRow::new(RowKind::Assistant, "done");
        row.thinking = "上方稳定块。".repeat(2_000);
        begin_row_scroll(&ctx, Some(10_000.0), false);
        let _ = ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(900.0, 600.0),
                )),
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| render_row(ui, &mut row, 1.0, true));
            },
        );
        let delta = end_row_scroll(&ctx);
        assert!(
            delta.is_finite() && delta.abs() > 1.0,
            "cold measurement must actually correct the anchor: {delta}"
        );
    }

    #[test]
    fn cached_layout_width_is_font_stable() {
        let ctx = egui::Context::default();
        install_fonts(&ctx);
        let text = "    let value = compute(argument);\n".repeat(500);
        let mut widths = Vec::new();
        for time in [1.0, 2.0] {
            let _ = ctx.run(Default::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    streamed_text(
                        ui,
                        ui.id().with("stable-width-test"),
                        &text,
                        true,
                        egui::FontId::monospace(13.0),
                        TEXT,
                    );
                    widths.push(ui.min_rect().width());
                });
                ctx.request_repaint_after(std::time::Duration::from_secs_f64(time));
            });
        }
        assert_eq!(widths[0], widths[1]);
    }
}
