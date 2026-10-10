//! Variable-height virtualization with logical viewport anchoring.
//! Only heavy layouts live in the bounded galley ring; cheap height metadata
//! survives eviction so revisiting text cannot change its geometry.
use crate::{cards, ChatRow, RowKind};
use eframe::egui;

#[derive(Default, Clone)]
struct Heights {
    width: u32,
    thinking: bool,
    values: Vec<f32>,
    prefix: Vec<f32>,
    measured: Vec<Option<f32>>,
    stamps: Vec<u64>,
    dragging_bar: bool,
    bar_band: Option<egui::Rect>,
    follow_tail: Option<bool>,
    last_offset: Option<f32>,
}
#[derive(Default, Clone)]
pub struct View {
    pub near_top: bool,
    #[cfg_attr(not(test),allow(dead_code))]
    pub first_visible: usize,
    pub first_visible_key: Option<u64>,
    pub at_bottom: bool,
    pub scrolling_down: bool,
}
fn view_id() -> egui::Id { egui::Id::new("feather-transcript-view") }
pub fn view(ctx:&egui::Context)->View {ctx.data(|d|d.get_temp::<View>(view_id()).unwrap_or_default())}
#[cfg(test)]
pub fn visible_start(ctx:&egui::Context)->usize {ctx.data(|d|d.get_temp::<View>(view_id()).map_or(0,|v|v.first_visible))}
fn prefix(values: &[f32]) -> Vec<f32> {
    let mut result = Vec::with_capacity(values.len() + 1);
    result.push(10.0);
    for height in values {
        result.push(result.last().unwrap() + height);
    }
    result
}
fn rebuild_prefix(values:&[f32],prefix:&mut Vec<f32>) {
    prefix.clear();prefix.reserve(values.len()+1);prefix.push(10.0);
    let mut total=10.0;
    for &height in values {total+=height;prefix.push(total);}
}
/// Return only the intersecting range with bounded pixel overscan.
fn visible_range(prefix: &[f32], top: f32, bottom: f32) -> std::ops::Range<usize> {
    let count = prefix.len().saturating_sub(1);
    let first = prefix
        .partition_point(|y| *y < top)
        .saturating_sub(1)
        .min(count);
    let end = prefix.partition_point(|y| *y <= bottom).min(count);
    first..end.max(first)
}
pub fn show(ui: &mut egui::Ui, rows: &mut [ChatRow], now: f64, thinking: bool, running: bool) -> bool {
    let id = ui.id().with("transcript-heights");
    let mut cache = ui
        .data_mut(|d| d.remove_temp::<Heights>(id))
        .unwrap_or_else(|| Heights {
            follow_tail: (std::env::var("FEATHER_UI_PREVIEW").ok().as_deref()==Some("1") && std::env::var("FEATHER_UI_PREVIEW_SCROLL").ok().as_deref()==Some("top")).then_some(false),
            ..Default::default()
        });
    let (pressed, down, pos, wheel) = ui.input(|i| {
        (
            i.pointer.primary_pressed(),
            i.pointer.primary_down(),
            i.pointer.interact_pos(),
            i.smooth_scroll_delta.y.abs() > 0.01,
        )
    });
    if !down {
        cache.dragging_bar = false;
    }
    if pressed && pos.zip(cache.bar_band).is_some_and(|(p, r)| r.contains(p)) {
        cache.dragging_bar = true;
    }
    let freeze = down; // Also freeze geometry during touch/content dragging.
    let follow = cache.follow_tail.unwrap_or(true) && !wheel && !down;
    // A sizing pass is discarded, never presented as a one-frame jump. Three
    // passes also cover a newly revealed neighbor after large estimate changes.
    ui.ctx()
        .options_mut(|o| o.max_passes = std::num::NonZeroUsize::new(3).unwrap());
    let mut correction = 0.0;
    let mut changed = false;
    let mut new_total = 0.0;
    let waiting = running && !rows.iter().any(|r| r.streaming);
    let mut first_visible=0usize;
    let mut first_visible_key=None;
    let output = egui::ScrollArea::vertical()
        .id_salt("conversation")
        .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible)
        .animated(false)
        .stick_to_bottom(false)
        .auto_shrink([false, false])
        .show_viewport(ui, |ui, viewport| {
            let available = ui.available_width().floor().max(1.0);
            let width = crate::theme::content_width(available);
            if cache.width != width as u32 || cache.thinking != thinking {
                cache.values.clear();
                cache.measured.clear();
                cache.stamps.clear();
                cache.width = width as u32;
                cache.thinking = thinking;
            }
            // Preserve old measurements and the viewport when a redb page is
            // prepended. ui_key is stable; row indices are not. This also handles
            // one boundary tool-result row being joined into its older call.
            let mut rebased_viewport=viewport;
            if !cache.stamps.is_empty() && cache.stamps[0]!=rows.first().map_or(0,|r|r.ui_key) {
                let old_prefix=prefix(&cache.values);
                let old_anchor=old_prefix.partition_point(|y|*y<=viewport.min.y).saturating_sub(1).min(cache.stamps.len()-1);
                let stamp=cache.stamps[old_anchor];
                let old_map:std::collections::HashMap<u64,(f32,Option<f32>)>=cache.stamps.iter().copied().zip(cache.values.iter().copied().zip(cache.measured.iter().copied())).collect();
                let mut values=Vec::with_capacity(rows.len());let mut measured=Vec::with_capacity(rows.len());
                for row in rows.iter(){let (h,m)=old_map.get(&row.ui_key).copied().unwrap_or((140.0,None));values.push(h);measured.push(m);}
                if let Some(new_anchor)=rows.iter().position(|r|r.ui_key==stamp) {
                    let next_prefix=prefix(&values);let added=next_prefix[new_anchor]-old_prefix[old_anchor];
                    correction+=added;rebased_viewport=viewport.translate(egui::vec2(0.0,added));
                    changed|=added.abs()>0.01;
                }
                cache.values=values;cache.measured=measured;cache.stamps=rows.iter().map(|r|r.ui_key).collect();
            }
            let viewport=rebased_viewport;
            cache.values.resize(rows.len(), 140.0);
            cache.measured.resize(rows.len(), None);
            cache.stamps.resize(rows.len(), 0);
            cache.values.truncate(rows.len());
            cache.measured.truncate(rows.len());
            cache.stamps.truncate(rows.len());
            for (index, row) in rows.iter().enumerate() {
                if cache.stamps[index] != row.ui_key {
                    cache.values[index] = 140.0;
                    cache.measured[index] = None;
                    cache.stamps[index] = row.ui_key;
                }
            }
            rebuild_prefix(&cache.values,&mut cache.prefix);
            let old_prefix = &cache.prefix;
            let anchor = old_prefix
                .partition_point(|y| *y <= viewport.min.y)
                .saturating_sub(1)
                .min(rows.len().saturating_sub(1));
            let range = visible_range(
                &old_prefix,
                (viewport.min.y - 1200.0).max(0.0),
                viewport.max.y + 1200.0,
            );
            first_visible=visible_range(&old_prefix,viewport.min.y.max(0.0),viewport.max.y).start.min(rows.len());
            first_visible_key=rows.get(first_visible).map(|row|row.ui_key);
            let origin = ui.cursor().min;
            let margin = ((available - width) * 0.5).floor().max(0.0);
            for index in range.clone() {
                let rect = egui::Rect::from_min_size(
                    origin + egui::vec2(margin, old_prefix[index]),
                    egui::vec2(width, 0.0),
                );
                let mut child = ui.new_child(
                    egui::UiBuilder::new()
                        .id_salt(rows[index].ui_key)
                        .max_rect(rect)
                        .layout(egui::Layout::top_down(egui::Align::Min)),
                );
                child.set_width(width);
                if freeze {
                    child.set_clip_rect(child.clip_rect().intersect(egui::Rect::from_min_size(
                        rect.min,
                        egui::vec2(width, cache.values[index]),
                    )));
                }
                cards::begin_row_scroll(
                    ui.ctx(),
                    (index == anchor).then_some(origin.y + viewport.min.y),
                    freeze,
                );
                cards::render_row(&mut child, &mut rows[index], now, thinking);
                let intra_correction = cards::end_row_scroll(ui.ctx());
                child.add_space(if rows[index].kind == RowKind::Tool {
                    7.0
                } else {
                    16.0
                });
                let height = child.min_rect().height().max(1.0);
                drop(child);
                cache.measured[index] = Some(height);
                if !freeze {
                    let delta = height - cache.values[index];
                    changed |= delta.abs() > 0.5;
                    if index < anchor {
                        correction += delta;
                    } else if index == anchor {
                        correction += intra_correction;
                    }
                    cache.values[index] = height;
                }
            }
            new_total = cache.values.iter().sum::<f32>() + 20.0 + if waiting { 76.0 } else { 0.0 };
            ui.set_min_size(egui::vec2(available, new_total));
            if waiting && range.end == rows.len() {
                let rect = egui::Rect::from_min_size(
                    origin + egui::vec2(margin, new_total - 76.0),
                    egui::vec2(width, 0.0),
                );
                let mut child =
                    ui.new_child(egui::UiBuilder::new().id_salt("waiting").max_rect(rect));
                child.set_width(width);
                crate::activity::waiting_card(&mut child, now);
            }
        });
    // egui applies wheel/drag input at ScrollArea::end. Preserve that movement
    // while rebasing the geometry; never fight its thumb mapping mid-drag.
    let mut state = output.state;
    let max_offset = (new_total - output.inner_rect.height()).max(0.0);
    let desired = if follow {
        max_offset
    } else {
        (state.offset.y + correction).clamp(0.0, max_offset)
    };
    if !freeze && (desired - state.offset.y).abs() > 0.01 {
        state.offset.y = desired;
        state.store(ui.ctx(), output.id);
        ui.ctx()
            .request_discard("transcript viewport anchor correction");
    } else if changed && !freeze {
        ui.ctx()
            .request_discard("transcript neighbor heights settled");
    }
    let at_bottom=!freeze && (state.offset.y - max_offset).abs() < 2.0;
    let scrolling_down=cache.last_offset.is_some_and(|previous|state.offset.y>previous+correction+1.0);
    cache.last_offset=Some(state.offset.y);
    cache.follow_tail = Some(at_bottom);
    cache.bar_band = Some(egui::Rect::from_min_max(
        egui::pos2(output.inner_rect.right(), output.inner_rect.top()),
        egui::pos2(output.inner_rect.right() + 24.0, output.inner_rect.bottom()),
    ));
    let near_top=!follow && !freeze && state.offset.y<240.0;
    ui.data_mut(|d| {
        d.insert_temp(id, cache);
        d.insert_temp(view_id(),View{near_top,first_visible,first_visible_key,at_bottom,scrolling_down});
    });
    near_top
}
#[cfg(test)]
mod tests {
    #[test]
    fn redb_page_prepend_preserves_visible_row_position_and_measurements() {
        let ctx=egui::Context::default();let mut rows:Vec<_>=(0..100).map(|n|ChatRow::new(RowKind::User,format!("kept row {n}"))).collect();
        let mut scroll=None;let mut cache_id=None;
        let mut frame=|rows:&mut Vec<ChatRow>| ctx.run(egui::RawInput{screen_rect:Some(egui::Rect::from_min_size(egui::Pos2::ZERO,egui::vec2(900.0,600.0))),..Default::default()},|ctx| {
            egui::CentralPanel::default().show(ctx,|ui| {scroll=Some(ui.make_persistent_id(egui::Id::new("conversation")));cache_id=Some(ui.id().with("transcript-heights"));show(ui,rows,0.0,false,false);});
        });
        for _ in 0..6{frame(&mut rows);}drop(frame);
        ctx.data_mut(|d| {let mut h=d.get_temp::<Heights>(cache_id.unwrap()).unwrap();h.follow_tail=Some(false);d.insert_temp(cache_id.unwrap(),h);});
        let mut state=egui::scroll_area::State::load(&ctx,scroll.unwrap()).unwrap();state.offset.y=1800.0;state.store(&ctx,scroll.unwrap());
        let frame=|rows:&mut Vec<ChatRow>|ctx.run(egui::RawInput{screen_rect:Some(egui::Rect::from_min_size(egui::Pos2::ZERO,egui::vec2(900.0,600.0))),..Default::default()},|ctx| {egui::CentralPanel::default().show(ctx,|ui|{show(ui,rows,0.0,false,false);});});
        let mut before=frame(&mut rows);for _ in 0..6{before=frame(&mut rows);}
        let target=before.shapes.iter().find_map(|s|match &s.shape {egui::Shape::Text(t) if t.pos.y>40.0&&t.pos.y<550.0&&t.galley.text().starts_with("kept row")=>Some((t.galley.text().to_owned(),t.pos.y)),_=>None}).unwrap();
        let mut prepended:Vec<_>=(0..25).map(|n|ChatRow::new(RowKind::User,format!("older page {n}"))).collect();prepended.append(&mut rows);rows=prepended;
        let after=frame(&mut rows);
        let y=after.shapes.iter().find_map(|s|match &s.shape{egui::Shape::Text(t) if t.galley.text()==target.0=>Some(t.pos.y),_=>None}).unwrap();
        assert!((y-target.1).abs()<1.5,"prepend moved visible row: before={} after={y}",target.1);
    }
    #[test]
    fn prefix_removal_preserves_visible_row_position_and_records_start() {
        let ctx=egui::Context::default();let mut rows:Vec<_>=(0..100).map(|n|ChatRow::new(RowKind::User,format!("kept row {n}"))).collect();
        let mut scroll=None;let mut cache_id=None;
        let mut frame=|rows:&mut Vec<ChatRow>| ctx.run(egui::RawInput{screen_rect:Some(egui::Rect::from_min_size(egui::Pos2::ZERO,egui::vec2(900.0,600.0))),..Default::default()},|ctx| {
            egui::CentralPanel::default().show(ctx,|ui| {scroll=Some(ui.make_persistent_id(egui::Id::new("conversation")));cache_id=Some(ui.id().with("transcript-heights"));show(ui,rows,0.0,false,false);});
        });
        for _ in 0..6{frame(&mut rows);}drop(frame);
        ctx.data_mut(|d| {let mut h=d.get_temp::<Heights>(cache_id.unwrap()).unwrap();h.follow_tail=Some(false);d.insert_temp(cache_id.unwrap(),h);});
        let mut state=egui::scroll_area::State::load(&ctx,scroll.unwrap()).unwrap();state.offset.y=1800.0;state.store(&ctx,scroll.unwrap());
        let frame=|rows:&mut Vec<ChatRow>|ctx.run(egui::RawInput{screen_rect:Some(egui::Rect::from_min_size(egui::Pos2::ZERO,egui::vec2(900.0,600.0))),..Default::default()},|ctx| {egui::CentralPanel::default().show(ctx,|ui|{show(ui,rows,0.0,false,false);});});
        let mut before=frame(&mut rows);for _ in 0..6{before=frame(&mut rows);}
        assert!(visible_start(&ctx)>0);
        let target=before.shapes.iter().find_map(|s|match &s.shape {egui::Shape::Text(t) if t.pos.y>40.0&&t.pos.y<550.0&&t.galley.text().starts_with("kept row")=>Some((t.galley.text().to_owned(),t.pos.y)),_=>None}).unwrap();
        drop(rows.drain(0..10));
        let after=frame(&mut rows);
        let y=after.shapes.iter().find_map(|s|match &s.shape{egui::Shape::Text(t) if t.galley.text()==target.0=>Some(t.pos.y),_=>None}).unwrap();
        assert!((y-target.1).abs()<1.5,"prefix removal moved visible row: before={} after={y}",target.1);
    }
    use super::*;
    #[test]
    fn range_is_bounded_for_large_histories() {
        let prefix: Vec<_> = (0..=10000).map(|i| i as f32 * 100.0).collect();
        assert_eq!(visible_range(&prefix, 998900.0, 999600.0), 9988..9997);
    }
    #[test]
    fn hidden_rows_do_not_submit_text_shapes() {
        let ctx = egui::Context::default();
        let mut rows: Vec<_> = (0..10000)
            .map(|i| ChatRow::new(RowKind::User, format!("history {i}")))
            .collect();
        let mut output = None;
        for frame in 0..5 {
            output = Some(ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(900.0, 600.0),
                    )),
                    time: Some(frame as f64 / 60.0),
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default()
                        .show(ctx, |ui| show(ui, &mut rows, 0.0, false, false));
                },
            ));
        }
        let texts = output
            .unwrap()
            .shapes
            .iter()
            .filter(|s| matches!(s.shape, egui::Shape::Text(_)))
            .count();
        assert!(texts < 60, "{texts} history labels submitted");
    }
    #[test]
    fn cold_measurement_above_viewport_is_compensated_before_present() {
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        crate::theme::apply_theme(&ctx);
        let mut rows: Vec<_> = (0..200)
            .map(|i| ChatRow::new(RowKind::Assistant, format!("ROW {i}")))
            .collect();
        let mut scroll_id = None;
        let mut cache_id = None;
        let mut run = |ctx: &egui::Context| {
            ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(900.0, 600.0),
                    )),
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        scroll_id = Some(ui.make_persistent_id(egui::Id::new("conversation")));
                        cache_id = Some(ui.id().with("transcript-heights"));
                        show(ui, &mut rows, 0.0, false, false);
                    });
                },
            )
        };
        for _ in 0..5 {
            run(&ctx);
        }
        drop(run);
        ctx.data_mut(|d| {
            let mut h = d.get_temp::<Heights>(cache_id.unwrap()).unwrap();
            h.follow_tail = Some(false);
            h.values.fill(140.0);
            h.measured.fill(None);
            d.insert_temp(cache_id.unwrap(), h);
        });
        let mut state = egui::scroll_area::State::load(&ctx, scroll_id.unwrap()).unwrap();
        state.offset.y = 14010.0;
        state.store(&ctx, scroll_id.unwrap());
        let mut run = || {
            ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(900.0, 600.0),
                    )),
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default()
                        .show(ctx, |ui| show(ui, &mut rows, 0.0, false, false));
                },
            )
        };
        let out = run();
        let row = out
            .shapes
            .iter()
            .find_map(|s| {
                if let egui::Shape::Text(t) = &s.shape {
                    (t.galley.text() == "ROW 100").then_some(t.pos.y)
                } else {
                    None
                }
            })
            .expect("same anchored row remains visible");
        let next = run();
        let row_next = next
            .shapes
            .iter()
            .find_map(|s| {
                if let egui::Shape::Text(t) = &s.shape {
                    (t.galley.text() == "ROW 100").then_some(t.pos.y)
                } else {
                    None
                }
            })
            .unwrap();
        assert!(
            (row - row_next).abs() < 0.5,
            "anchor moved after cold sizing: {row} -> {row_next}"
        );
        assert!(
            egui::scroll_area::State::load(&ctx, scroll_id.unwrap())
                .unwrap()
                .offset
                .y
                < 14010.0,
            "measured shorter rows above viewport must rebase offset"
        );
    }
}
