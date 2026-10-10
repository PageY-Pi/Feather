//! Real wheel and scrollbar events, including cold height estimates.
use super::*;
use eframe::egui;
struct Harness {
    ctx: egui::Context,
    rows: Vec<ChatRow>,
    id: Option<egui::Id>,
    cache_id: Option<egui::Id>,
    time: f64,
}
impl Harness {
    fn new(rows: Vec<ChatRow>) -> Self {
        let ctx = egui::Context::default();
        theme::install_fonts(&ctx);
        theme::apply_theme(&ctx);
        Self {
            ctx,
            rows,
            id: None,
            cache_id: None,
            time: 0.0,
        }
    }
    fn frame(&mut self, events: Vec<egui::Event>) -> egui::FullOutput {
        self.time += 1.0 / 60.0;
        let time = self.time;
        let rows = &mut self.rows;
        let id = &mut self.id;
        let cache_id = &mut self.cache_id;
        self.ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(900.0, 600.0),
                )),
                time: Some(time),
                events,
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    *id = Some(ui.make_persistent_id(egui::Id::new("conversation")));
                    *cache_id = Some(ui.id().with("transcript-heights"));
                    transcript::show(ui, rows, time, true, false);
                });
            },
        )
    }
    fn offset(&self) -> f32 {
        egui::scroll_area::State::load(&self.ctx, self.id.unwrap())
            .unwrap()
            .offset
            .y
    }
    fn wheel(&mut self, dy: f32) -> egui::FullOutput {
        self.frame(vec![
            egui::Event::PointerMoved(egui::pos2(400.0, 200.0)),
            egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: egui::vec2(0.0, dy),
                modifiers: Default::default(),
            },
        ])
    }
    fn idle(&mut self) -> egui::FullOutput {
        self.frame(vec![])
    }
}
fn geometry(out: &egui::FullOutput) -> Vec<(String, egui::Pos2)> {
    out.shapes
        .iter()
        .filter_map(|s| {
            if let egui::Shape::Text(t) = &s.shape {
                Some((t.galley.text().into(), t.pos))
            } else {
                None
            }
        })
        .collect()
}
#[test]
fn cold_wheel_rows_settle_before_present_and_reverse_is_stable() {
    let rows = (0..300)
        .map(|i| ChatRow::new(RowKind::Assistant, format!("ROW {i} short message")))
        .collect();
    let mut h = Harness::new(rows);
    for _ in 0..8 {
        h.idle();
    }
    for _ in 0..60 {
        let out = h.wheel(100.0);
        // Input smoothing legitimately persists after the physical wheel event.
        for _ in 0..45 {
            h.idle();
        }
        let settled = h.idle();
        let still = h.idle();
        assert_eq!(
            geometry(&settled),
            geometry(&still),
            "cold row sizing must not creep after scrolling"
        );
        assert!(!geometry(&out).is_empty());
    }
    for _ in 0..20 {
        h.wheel(-130.0);
        for _ in 0..45 {
            h.idle();
        }
        let a = h.idle();
        let b = h.idle();
        assert_eq!(geometry(&a), geometry(&b));
    }
}
#[test]
fn long_thinking_scroll_and_cache_eviction_preserve_measured_geometry() {
    let mut row = ChatRow::new(RowKind::Assistant, "done");
    row.thinking = (0..6000)
        .map(|i| format!("analysis {i}: 中文推理与长段落，滚动锚点必须保持稳定。\n"))
        .collect();
    let mut h = Harness::new(vec![row]);
    for _ in 0..12 {
        h.idle();
    }
    for _ in 0..45 {
        h.wheel(200.0);
        for _ in 0..10 {
            h.idle();
        }
    }
    for _ in 0..45 {
        h.idle();
    }
    let a = h.idle();
    let offset = h.offset();
    let b = h.idle();
    assert_eq!(geometry(&a), geometry(&b));
    assert!(!geometry(&a).is_empty());
    cards::clear_layout_ring_for_test(&h.ctx);
    let c = h.idle();
    assert_eq!(geometry(&a),geometry(&c),"eviction cannot move already visible text");
    let d = h.idle();
    assert_eq!(
        geometry(&c),
        geometry(&d),
        "ring eviction must preserve layout heights"
    );
    assert!((h.offset() - offset).abs() < 0.5);
}
#[test]
fn scrollbar_drag_keeps_extent_frozen_until_release() {
    let mut h = Harness::new(
        (0..300)
            .map(|i| ChatRow::new(RowKind::Assistant, format!("ROW {i} short")))
            .collect(),
    );
    for _ in 0..8 {
        h.idle();
    }
    // Full-height bar on the right; pressing the track and moving thumb exercises
    // egui's real interaction, not programmatically changing State.offset.
    let p = egui::pos2(887.0, 300.0);
    h.frame(vec![
        egui::Event::PointerMoved(p),
        egui::Event::PointerButton {
            pos: p,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: Default::default(),
        },
    ]);
    let down_offset = h.offset();
    assert!(down_offset > 0.0);
    for y in [320.0, 350.0, 400.0, 450.0, 350.0, 240.0] {
        h.frame(vec![egui::Event::PointerMoved(egui::pos2(887.0, y))]);
        let offset = h.offset();
        let a = h.idle();
        let b = h.idle();
        assert!(
            (h.offset() - offset).abs() < 0.5,
            "stationary thumb must not move as cold rows measure"
        );
        assert_eq!(geometry(&a), geometry(&b));
    }
    h.frame(vec![egui::Event::PointerButton {
        pos: egui::pos2(887.0, 240.0),
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: Default::default(),
    }]);
    let a = h.idle();
    let b = h.idle();
    assert_eq!(
        geometry(&a),
        geometry(&b),
        "release must commit geometry without visible follow-up twitch"
    );
}
