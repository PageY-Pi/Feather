use super::*;

fn header_frame(
    app: &mut FeatherApp,
    ctx: &egui::Context,
    width: f32,
    maximized: bool,
    events: Vec<egui::Event>,
) -> egui::FullOutput {
    let mut input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(width, 560.0),
        )),
        events,
        ..Default::default()
    };
    input
        .viewports
        .get_mut(&egui::ViewportId::ROOT)
        .unwrap()
        .maximized = Some(maximized);
    ctx.run(input, |ctx| {
        egui::TopBottomPanel::top("header-test")
            .frame(egui::Frame::none())
            .show(ctx, |ui| app.top_bar(ui));
    })
}
fn click(pos: egui::Pos2, pressed: bool) -> Vec<egui::Event> {
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
fn text_bounds(out: &egui::FullOutput, label: &str) -> (egui::Rect, egui::Rect) {
    out.shapes
        .iter()
        .find_map(|s| match &s.shape {
            egui::Shape::Text(t) if t.galley.text() == label => {
                Some((t.galley.mesh_bounds.translate(t.pos.to_vec2()), s.clip_rect))
            }
            _ => None,
        })
        .unwrap_or_else(|| panic!("missing visible label: {label}"))
}

#[test]
fn long_paths_do_not_displace_header_controls_at_any_supported_width() {
    for width in [760.0, 900.0, 1160.0, 1920.0] {
        let ctx = egui::Context::default();
        theme::apply_theme(&ctx);
        let mut app = tests::fixture();
        app.root = std::path::PathBuf::from(format!(
            "C:/workspace/{}/{}",
            "very-long-directory".repeat(20),
            "project".repeat(60)
        ));
        for _ in 0..3 {
            header_frame(&mut app, &ctx, width, false, Vec::new());
        }
        let out = header_frame(&mut app, &ctx, width, false, Vec::new());
        let (bounds, clip) = text_bounds(&out, "Add API");
        assert!(clip.contains_rect(bounds));
        assert!(bounds.left() > width - 272.0 && bounds.right() < width - 150.0);
        assert!(bounds.top() >= 0.0 && bounds.bottom() <= 60.0);
        let path_clip = out
            .shapes
            .iter()
            .find_map(|s| match &s.shape {
                egui::Shape::Text(t) if t.galley.text().starts_with("C:") => Some(s.clip_rect),
                _ => None,
            })
            .expect("workspace path rendered");
        assert!(path_clip.right() < bounds.left() - 20.0);
        assert_eq!(ctx.used_rect().height(), 60.0);
    }
}

#[test]
fn header_buttons_send_correct_commands_and_add_opens_model_editor() {
    let width = 760.0;
    for (offset, maximized, expected) in [
        (0.0, false, egui::ViewportCommand::Close),
        (40.0, false, egui::ViewportCommand::Maximized(true)),
        (40.0, true, egui::ViewportCommand::Maximized(false)),
        (80.0, false, egui::ViewportCommand::Minimized(true)),
    ] {
        let ctx = egui::Context::default();
        let mut app = tests::fixture();
        for _ in 0..3 {
            header_frame(&mut app, &ctx, width, maximized, Vec::new());
        }
        let pos = egui::pos2(width - 30.0 - offset, 30.0);
        header_frame(&mut app, &ctx, width, maximized, click(pos, true));
        let out = header_frame(&mut app, &ctx, width, maximized, click(pos, false));
        let out = if expected == egui::ViewportCommand::Close {
            assert!(
                app.exit.is_some(),
                "header close must use the shared cancellation lifecycle"
            );
            ctx.run(Default::default(), |ctx| app.poll_exit(ctx))
        } else {
            out
        };
        assert!(
            out.viewport_output[&egui::ViewportId::ROOT]
                .commands
                .contains(&expected),
            "offset {offset} maximized={maximized}: {:?}",
            out.viewport_output[&egui::ViewportId::ROOT].commands
        );
        assert!(!app.show_login);
    }
    let ctx = egui::Context::default();
    let mut app = tests::fixture();
    for _ in 0..3 {
        header_frame(&mut app, &ctx, width, false, Vec::new());
    }
    let pos = egui::pos2(width - 208.0, 30.0);
    header_frame(&mut app, &ctx, width, false, click(pos, true));
    let out = header_frame(&mut app, &ctx, width, false, click(pos, false));
    assert!(app.show_login && app.model_manager.enabled);
    assert!(
        out.viewport_output[&egui::ViewportId::ROOT]
            .commands
            .is_empty(),
        "Add API must not drag, maximize, or close"
    );
}

#[test]
fn close_hover_has_destructive_feedback_and_controls_have_equal_targets() {
    let ctx = egui::Context::default();
    theme::apply_theme(&ctx);
    let rect = egui::Rect::from_min_size(egui::pos2(20.0, 20.0), egui::vec2(36.0, 34.0));
    for _ in 0..2 {
        let _ = ctx.run(Default::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                window_chrome::control(ui, rect, window_chrome::Control::Close);
            });
        });
    }
    let out = ctx.run(
        egui::RawInput {
            events: vec![egui::Event::PointerMoved(rect.center())],
            ..Default::default()
        },
        |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                assert_eq!(
                    window_chrome::control(ui, rect, window_chrome::Control::Close)
                        .rect
                        .size(),
                    egui::vec2(36.0, 34.0)
                );
            });
        },
    );
    assert!(out.shapes.iter().any(|s| matches!(&s.shape,egui::Shape::LineSegment{stroke,..} if stroke.color==eframe::epaint::ColorMode::Solid(theme::RED))));
    assert!(out.shapes.iter().any(|s| matches!(&s.shape,egui::Shape::Rect(r) if r.rect==rect && r.fill!=egui::Color32::TRANSPARENT)));
}

#[test]
fn dialog_actions_remain_visible_and_validation_error_is_not_inside_scroll() {
    for size in [
        egui::vec2(1160.0, 820.0),
        egui::vec2(760.0, 560.0),
        egui::vec2(760.0, 520.0),
    ] {
        let ctx = egui::Context::default();
        theme::apply_theme(&ctx);
        let mut app = tests::fixture();
        app.show_login = true;
        app.model_manager.enabled = true;
        app.model_manager.error = Some("Validation test error".into());
        for _ in 0..4 {
            let _ = ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
                    ..Default::default()
                },
                |ctx| app.frame_ui(ctx),
            );
        }
        let out = ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
                ..Default::default()
            },
            |ctx| app.frame_ui(ctx),
        );
        for label in ["Save model", "Clear form", "Validation test error"] {
            let (bounds, clip) = text_bounds(&out, label);
            assert!(
                clip.contains_rect(bounds),
                "{label} clipped at {size:?}: {bounds:?} in {clip:?}"
            );
            assert!(
                bounds.top() > 40.0 && bounds.bottom() < size.y - 12.0,
                "{label} must fit in the viewport"
            );
        }
    }
}

#[test]
fn footer_long_ids_and_unicode_ellipsis_stay_bounded() {
    assert_eq!(ellipsize("想法是完整的", 4), "想法是…");
    assert_eq!(ellipsize("abc", 3), "abc");
    let ctx = egui::Context::default();
    theme::apply_theme(&ctx);
    let mut app = tests::fixture();
    app.settings.model = format!("custom/{}", "long-model-name".repeat(40));
    app.root = "long-workspace-name-with-unicode-想法".into();
    for _ in 0..3 {
        let _ = ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(510.0, 120.0),
                )),
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| app.status_bar(ui));
            },
        );
    }
    assert!(ctx.used_rect().width() <= 510.0);
}

#[test]
fn header_focus_order_matches_visual_order_and_keyboard_activation_works() {
    let ctx = egui::Context::default();
    let mut app = tests::fixture();
    let mut ids = Vec::new();
    for _ in 0..3 {
        let _ = ctx.run(Default::default(), |ctx| {
            egui::TopBottomPanel::top("focus-header").show(ctx, |ui| {
                ids = vec![
                    window_chrome::add_id(ui),
                    window_chrome::control_id(ui, window_chrome::Control::Minimize),
                    window_chrome::control_id(ui, window_chrome::Control::Maximize),
                    window_chrome::control_id(ui, window_chrome::Control::Close),
                ];
                app.top_bar(ui);
            });
        });
    }
    ctx.memory_mut(|m| m.request_focus(ids[0]));
    for expected in &ids[1..] {
        let event = egui::Event::Key {
            key: egui::Key::Tab,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        };
        let _ = ctx.run(
            egui::RawInput {
                events: vec![event],
                ..Default::default()
            },
            |ctx| {
                egui::TopBottomPanel::top("focus-header").show(ctx, |ui| app.top_bar(ui));
            },
        );
        assert_eq!(ctx.memory(|m| m.focused()), Some(*expected));
    }
    let event = egui::Event::Key {
        key: egui::Key::Enter,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::NONE,
    };
    let out = ctx.run(
        egui::RawInput {
            events: vec![event],
            ..Default::default()
        },
        |ctx| {
            egui::TopBottomPanel::top("focus-header").show(ctx, |ui| app.top_bar(ui));
        },
    );
    assert!(
        app.exit.is_some(),
        "keyboard close must request cooperative cancellation"
    );
    let out = ctx.run(Default::default(), |ctx| app.poll_exit(ctx));
    assert!(out.viewport_output[&egui::ViewportId::ROOT]
        .commands
        .contains(&egui::ViewportCommand::Close));
}

#[test]
fn preview_model_and_auth_actions_never_persist_or_start_browser_login() {
    let mut app = tests::fixture();
    let provider: models::ProviderEntry = serde_json::from_str("{\"models\":[]}").unwrap();
    let model: models::ModelEntry = serde_json::from_str("{\"id\":\"preview-model\"}").unwrap();
    app.manage_model(model_manager::Action::Save(
        "preview-provider".into(),
        provider,
        model,
    ));
    assert!(app
        .model_manager
        .error
        .as_deref()
        .unwrap()
        .contains("read-only"));
    app.spawn_login(&oauth::PROVIDERS[0]);
    app.spawn_auth_update("zai", Some(oauth::OAuthAccount::api_key("synthetic-value")));
    assert!(app.auth_pending.is_empty());
    assert!(app.ev_rx.is_empty());
}
