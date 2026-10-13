use super::*;
#[test]
fn skill_command_lazy_expansion_uses_current_file_and_unknown_draft_is_kept() {
    let mut app = tests::fixture();
    let home = std::env::temp_dir().join(feather_core::provider::gen_id("gui-skills"));
    let root = home.join("root");
    let skill = root.join(".feather/skills/demo");
    std::fs::create_dir_all(&skill).unwrap();
    app.root = root.clone();
    app.home = home.clone();
    app.execute_command("/skill");
    assert!(app.request_picker.is_some());
    assert!(app.rows.is_empty());
    app.request_picker = None;
    app.input = "/skill missing request".into();
    app.submit();
    assert_eq!(app.input, "/skill missing request");
    assert!(!app.running);
    assert!(app.rows.last().unwrap().text.contains("unknown skill"));
    std::fs::write(
        skill.join("SKILL.md"),
        "---\nname: demo\ndescription: demo description\n---\nPRIVATE INSTRUCTION",
    )
    .unwrap();
    let resources = config::resources_for(&root, &home);
    assert!(!resources.skill_listing().contains("PRIVATE INSTRUCTION"));
    let agent = tests::offline_agent();
    app.input_queue = agent.input_queue();
    app.running = true;
    app.input = "/skill:demo do work".into();
    app.submit();
    let mut agent = agent;
    app.rt.block_on(async {
        agent.prompt("first").await.unwrap();
    });
    assert!(agent
        .session()
        .messages()
        .iter()
        .any(|m| m.text().contains("PRIVATE INSTRUCTION") && m.text().contains("do work")));
    app.running = false;
    app.open_request_picker(false);
    let ctx = app.egui_ctx.clone();
    app.handle_action("app.interrupt", &ctx);
    assert!(app.request_picker.is_none());
    drop(app);
    std::fs::remove_dir_all(home).unwrap();
}

#[test]
fn proxy_footer_click_opens_chooser_without_touching_real_config() {
    let mut app = tests::fixture();
    let ctx = app.egui_ctx.clone();
    theme::apply_theme(&ctx);
    let mut run = |events| {
        ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(950.0, 180.0),
                )),
                events,
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| app.status_bar(ui));
            },
        )
    };
    for _ in 0..3 {
        run(Vec::new());
    }
    let out = run(Vec::new());
    let pos = out
        .shapes
        .iter()
        .find_map(|s| match &s.shape {
            egui::Shape::Text(t) if t.galley.text() == "Proxy: Off" => {
                Some(t.pos + t.galley.rect.center().to_vec2())
            }
            _ => None,
        })
        .unwrap();
    let click = |pressed| {
        vec![
            egui::Event::PointerMoved(pos),
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            },
        ]
    };
    run(click(true));
    run(click(false));
    drop(run);
    assert!(matches!(
        app.request_picker,
        Some(request_choices::Picker::Proxy { .. })
    ));
    let draft = app.input.clone();
    let _ = ctx.run(
        egui::RawInput {
            events: vec![egui::Event::Key {
                key: egui::Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
            ..Default::default()
        },
        |ctx| app.frame_ui(ctx),
    );
    assert!(app.request_picker.is_none());
    assert_eq!(app.input, draft);
}
