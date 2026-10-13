use crate::tests::fixture;
use crate::*;

#[test]
fn slash_help_executes_locally_and_menu_lists_descriptions() {
    let mut app = fixture();
    app.input = "/".into();
    let _ = app.egui_ctx.clone().run(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1160.0, 820.0),
            )),
            ..Default::default()
        },
        |ctx| app.frame_ui(ctx),
    );
    let ctx = app.egui_ctx.clone();
    let output = ctx.run(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1160.0, 820.0),
            )),
            ..Default::default()
        },
        |ctx| app.frame_ui(ctx),
    );
    let texts: Vec<&str> = output
        .shapes
        .iter()
        .filter_map(|s| match &s.shape {
            egui::Shape::Text(t) => Some(t.galley.job.text.as_str()),
            _ => None,
        })
        .collect();
    assert!(texts.iter().any(|s| s.contains("Commands")));
    assert!(texts.contains(&"/new"));
    app.input = "/help".into();
    app.submit();
    assert!(!app.running);
    assert!(app.agent.is_none());
    assert!(app.rows.last().unwrap().text.contains("/rewind"));
}

#[test]
fn escape_registry_cancel_closes_all_pickers() {
    let mut app = fixture();
    let ctx = app.egui_ctx.clone();
    let action =
        keybindings::egui_event_to_action(&app.kb, egui::Modifiers::NONE, egui::Key::Escape)
            .unwrap();
    app.show_models = true;
    app.show_thinking_picker = true;
    app.show_login = true;
    app.handle_action(action, &ctx);
    assert!(!app.show_models && !app.show_thinking_picker && !app.show_login);
}

#[test]
fn footer_entry_opens_one_picker_at_a_time() {
    let mut app = fixture();
    app.toggle_models();
    assert!(app.show_models);
    app.open_thinking_picker();
    assert!(app.show_thinking_picker && !app.show_models);
}

#[test]
fn oauth_visibility_requires_usable_credentials_but_keeps_custom_providers() {
    let mut app = fixture();
    let p: models::ProviderEntry = serde_json::from_str("{\"models\":[]}").unwrap();
    assert!(!app.provider_available("openai-codex", &p));
    assert!(app.provider_available("my-local-server", &p));
    app.auth.set(
        "openai-codex",
        oauth::OAuthAccount {
            kind: "oauth".into(),
            refresh: "synthetic-refresh".into(),
            expires: 1,
            ..Default::default()
        },
    );
    assert!(app.provider_available("openai-codex", &p));
    app.auth.set(
        "openai-codex",
        oauth::OAuthAccount {
            kind: "oauth".into(),
            access: "expired".into(),
            expires: 1,
            ..Default::default()
        },
    );
    assert!(!app.provider_available("openai-codex", &p));
    app.auth
        .set("zai", oauth::OAuthAccount::api_key("synthetic-key"));
    assert!(app.provider_available("zai", &p));
    let p: models::ProviderEntry = serde_json::from_str("{\"apiKey\":\"synthetic-key\"}").unwrap();
    assert!(app.provider_available("anthropic", &p));
    let p: models::ProviderEntry =
        serde_json::from_str("{\"apiKey\":\"$FEATHER_TEST_DEFINITELY_UNSET_9281\"}").unwrap();
    assert!(!app.provider_available("anthropic", &p));
}

#[test]
fn effort_options_follow_selected_model() {
    let mut app = fixture();
    app.catalog = Some(
        serde_json::from_str(
            r#"{"providers":{"local":{"models":[
        {"id":"reasoner","reasoning":true,"thinkingLevels":["off","low","high"]},
        {"id":"plain","reasoningNotSupported":true}]}}}"#,
        )
        .unwrap(),
    );
    app.settings.model = "local/reasoner".into();
    assert_eq!(
        app.effort_levels(),
        vec!["off", "low", "medium", "high", "xhigh", "max"]
    );
    app.settings.model = "local/plain".into();
    assert_eq!(app.effort_levels(), vec!["off"]);
}

#[test]
fn egui_footer_pointer_clicks_open_model_and_effort_windows() {
    let mut app = fixture();
    app.settings.model = "local/reasoner".into();
    let ctx = app.egui_ctx.clone();
    crate::theme::install_fonts(&ctx);
    crate::theme::apply_theme(&ctx);
    let mut frame = |events: Vec<egui::Event>| {
        ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(900.0, 700.0),
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
        frame(Vec::new());
    }
    let output = frame(Vec::new());
    let position = |output: &egui::FullOutput, label: &str| {
        output
            .shapes
            .iter()
            .find_map(|s| match &s.shape {
                egui::Shape::Text(t) if t.galley.text() == label => {
                    Some(t.pos + t.galley.mesh_bounds.center().to_vec2())
                }
                _ => None,
            })
            .expect("footer label exists")
    };
    let model = position(&output, "reasoner");
    let click = |pos, pressed| {
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
    frame(click(model, true));
    let output = frame(click(model, false));
    let thinking = position(&output, "thinking off");
    frame(click(thinking, true));
    frame(click(thinking, false));
    drop(frame);
    assert!(app.show_thinking_picker && !app.show_models);
}

fn session_info(id: &str, prompt: &str) -> feather_core::session_catalog::SessionInfo {
    feather_core::session_catalog::SessionInfo {
        id: id.into(),
        dir: std::path::PathBuf::from(format!("sessions/{id}")),
        modified_ms: 1,
        first_prompt: prompt.into(),
        created_ms: 1,
        cwd: std::path::PathBuf::from("fixture"),
    }
}

#[test]
fn session_pointer_hover_moves_highlight_without_resuming() {
    let ctx = egui::Context::default();
    let mut open = true;
    let mut picker = sessions::SessionChooser::new(vec![
        session_info("s1", "first"),
        session_info("s2", "second"),
    ]);
    let input = |events| egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(1160.0, 820.0),
        )),
        events,
        ..Default::default()
    };
    let mut pick = None;
    let _ = ctx.run(input(vec![]), |ctx| {
        pick = sessions::picker_window(ctx, &mut picker, &mut open)
    });
    let output = ctx.run(input(vec![]), |ctx| {
        pick = sessions::picker_window(ctx, &mut picker, &mut open)
    });
    let pos = output
        .shapes
        .iter()
        .find_map(|s| match &s.shape {
            egui::Shape::Text(t) if t.galley.text() == "second" => {
                Some(t.pos + t.galley.mesh_bounds.center().to_vec2())
            }
            _ => None,
        })
        .unwrap();
    for _ in 0..2 {
        let _ = ctx.run(input(vec![egui::Event::PointerMoved(pos)]), |ctx| {
            pick = sessions::picker_window(ctx, &mut picker, &mut open)
        });
    }
    assert_eq!(picker.active_row(), 2);
    assert!(pick.is_none());
}

#[test]
fn resume_shortcut_is_bound_to_ctrl_shift_r() {
    let kb =
        keybindings::Keybindings::load_from(std::path::Path::new("missing-test-bindings.json"));
    let mods = egui::Modifiers {
        ctrl: true,
        shift: true,
        command: true,
        ..Default::default()
    };
    assert_eq!(
        keybindings::egui_event_to_action(&kb, mods, egui::Key::R),
        Some("app.session.resume")
    );
}

#[test]
fn startup_chooser_lists_history_without_materializing_an_agent() {
    let mut app = fixture();
    app.show_picker(vec![
        session_info("s-1", "first task"),
        session_info("s-2", "second task"),
    ]);
    let picker = app.picker.as_ref().expect("picker visible");
    assert_eq!(picker.row_count(), 3, "new row plus two sessions");
    assert!(app.agent.is_none(), "no durable session before a choice");
    assert!(app.rows.is_empty());
}

#[test]
fn choosing_new_session_discards_transcript_and_stays_agent_less() {
    let mut app = fixture();
    app.show_picker(vec![session_info("s-1", "first task")]);
    app.rows
        .push(ChatRow::new(RowKind::User, "stale transcript"));
    app.choose_pick(sessions::Pick::NewSession);
    assert!(app.picker.is_none());
    assert!(app.rows.is_empty(), "new welcome never restores history");
    assert!(app.agent.is_none());
}

#[test]
fn picker_closes_competing_modals() {
    let mut app = fixture();
    app.show_models = true;
    app.show_picker(vec![session_info("s-1", "first task")]);
    assert!(!app.show_models);
    assert!(app.picker.is_some());
}

#[test]
fn switching_sessions_is_refused_while_running() {
    let mut app = fixture();
    app.show_picker(vec![session_info("s-1", "first task")]);
    app.running = true;
    app.choose_pick(sessions::Pick::Resume("sessions/s-1".into()));
    assert!(app.picker.is_some(), "an in-flight run keeps the picker");
    assert!(app.agent.is_none());
}

#[test]
fn resume_action_toggles_the_chooser() {
    let mut app = fixture();
    app.show_picker(vec![session_info("s-1", "first task")]);
    let ctx = app.egui_ctx.clone();
    app.handle_action("app.session.resume", &ctx);
    assert!(
        app.picker.is_none(),
        "second resume action closes the picker"
    );
}

#[test]
fn session_new_shortcut_clears_rows_without_a_durable_session() {
    let mut app = fixture();
    app.rows.push(ChatRow::new(RowKind::User, "old"));
    let ctx = app.egui_ctx.clone();
    app.handle_action("app.session.new", &ctx);
    assert!(app.rows.is_empty());
    assert!(app.agent.is_none());
}
