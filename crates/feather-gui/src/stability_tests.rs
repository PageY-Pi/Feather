use super::*;
fn app() -> FeatherApp {
    crate::tests::fixture()
}
fn frame(app: &mut FeatherApp, time: f64, events: Vec<egui::Event>) -> egui::FullOutput {
    let ctx = app.egui_ctx.clone();
    ctx.run(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1160.0, 820.0),
            )),
            time: Some(time),
            events,
            ..Default::default()
        },
        |ctx| app.frame_ui(ctx),
    )
}
fn chat_geometry(output: &egui::FullOutput) -> Vec<(String, egui::Pos2, egui::Vec2)> {
    output
        .shapes
        .iter()
        .filter_map(|shape| {
            if let egui::Shape::Text(t) = &shape.shape {
                let text = t.galley.text();
                (text.starts_with("question") || text.starts_with("Answer"))
                    .then(|| (text.to_owned(), t.pos, t.galley.size()))
            } else {
                None
            }
        })
        .collect()
}
#[test]
fn caret_blink_movement_and_typing_cannot_reflow_transcript() {
    let mut app = app();
    let ctx = app.egui_ctx.clone();
    theme::install_fonts(&ctx);
    theme::apply_theme(&ctx);
    for i in 0..100 {
        app.rows
            .push(ChatRow::new(RowKind::User, format!("question {i}")));
        app.rows.push(ChatRow::new(
            RowKind::Assistant,
            format!("Answer {i} 中文分析，布局保持稳定。").repeat(4),
        ));
    }
    app.input = "long draft line 中文输入\n".repeat(40);
    let id = egui::Id::new("feather-composer");
    ctx.memory_mut(|m| m.request_focus(id));
    for i in 0..20 {
        frame(&mut app, i as f64 / 60.0, vec![]);
    }
    let baseline = chat_geometry(&frame(&mut app, 1.0, vec![]));
    assert!(!baseline.is_empty());
    assert!(app
        .egui_ctx
        .memory(|m| m.has_focus(egui::Id::new("feather-composer"))));
    let input_len = app.input.len();
    for i in 0..12 {
        if let Some(mut state) = egui::text_edit::TextEditState::load(&ctx, id) {
            state
                .cursor
                .set_char_range(Some(egui::text::CCursorRange::one(
                    egui::text::CCursor::new(i * 20),
                )));
            state.store(&ctx, id);
        }
        let output = frame(&mut app, 1.5 + i as f64 * 0.5, vec![]);
        assert_eq!(
            chat_geometry(&output),
            baseline,
            "caret/blink moved history at {i}"
        );
    }
    for i in 0..5 {
        let output = frame(
            &mut app,
            8.0 + i as f64 / 60.0,
            vec![egui::Event::Text("x".into())],
        );
        assert_eq!(
            chat_geometry(&output),
            baseline,
            "typing moved history at {i}"
        );
    }
    assert_eq!(
        app.input.len(),
        input_len + 5,
        "typing must reach the composer during the regression"
    );
}
#[test]
fn large_history_emits_only_visible_text_on_caret_frames() {
    let mut app = app();
    theme::install_fonts(&app.egui_ctx);
    theme::apply_theme(&app.egui_ctx);
    for i in 0..10000 {
        app.rows
            .push(ChatRow::new(RowKind::User, format!("question {i}")));
    }
    let start = std::time::Instant::now();
    let mut output = None;
    for i in 0..10 {
        output = Some(frame(&mut app, i as f64 / 60.0, vec![]));
    }
    let count = output
        .unwrap()
        .shapes
        .iter()
        .filter(|s| matches!(s.shape, egui::Shape::Text(_)))
        .count();
    assert!(count < 100, "submitted {count} historical text shapes");
    eprintln!("10000-row frame mean {:?}", start.elapsed() / 10);
}
