use super::*;
#[test]
fn ordinary_long_draft_does_not_become_a_duplicate_menu_cache_key() {
    let mut app = tests::fixture();
    app.input = "long draft ".repeat(100_000);
    let first = app.cached_suggestions();
    assert!(first.is_empty());
    drop(first);
    app.input.push_str("more");
    let second = app.cached_suggestions();
    assert!(second.is_empty());
}
#[test]
fn inline_skill_and_turn_rows_are_clickable() {
    let home = std::env::temp_dir().join(feather_core::provider::gen_id("menu-pointer"));
    let root = home.join("workspace");
    let dir = home.join("skills/demo");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("SKILL.md"),
        "---\nname: demo\ndescription: Demo request\n---\nBODY",
    )
    .unwrap();
    let mut app = tests::fixture();
    app.home = home.clone();
    app.root = root;
    app.input = "/skill ".into();
    let ctx = app.egui_ctx.clone();
    theme::apply_theme(&ctx);
    let mut frame = |events| {
        ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(950.0, 820.0),
                )),
                events,
                ..Default::default()
            },
            |ctx| app.frame_ui(ctx),
        )
    };
    let mut out = frame(Vec::new());
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let label_pos = |out: &egui::FullOutput, label: &str| {
        out.shapes.iter().find_map(|s| {
            if let egui::Shape::Text(text) = &s.shape {
                (text.galley.text() == label)
                    .then_some(text.pos + text.galley.rect.center().to_vec2())
            } else {
                None
            }
        })
    };
    let pos = loop {
        if let Some(pos) = label_pos(&out, "/skill:demo ") {
            break pos;
        }
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(5));
        out = frame(Vec::new());
    };
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
    frame(click(pos, true));
    frame(click(pos, false));
    drop(frame);
    assert_eq!(app.input, "/skill:demo ");
    assert!(!app.running);
    assert!(app.rows.is_empty());
    app.input = "/rewind ".into();
    app.rewind_turns.loaded = true;
    app.rewind_turns.rows = vec![(1, "First request".into())];
    let mut frame = |events| {
        ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(950.0, 820.0),
                )),
                events,
                ..Default::default()
            },
            |ctx| app.frame_ui(ctx),
        )
    };
    for _ in 0..3 {
        frame(Vec::new());
    }
    let pos = label_pos(&frame(Vec::new()), "/rewind 1").unwrap();
    frame(click(pos, true));
    frame(click(pos, false));
    drop(frame);
    assert!(app.input.is_empty());
    assert!(app.rows.last().unwrap().text.contains("No active session"));
    drop(app);
    std::fs::remove_dir_all(home).unwrap();
}
#[test]
fn composer_has_no_send_button_but_keeps_enter_hint() {
    let mut app = tests::fixture();
    let ctx = app.egui_ctx.clone();
    theme::apply_theme(&ctx);
    let mut texts = Vec::new();
    fn gather(shape: &egui::Shape, texts: &mut Vec<String>) {
        match shape {
            egui::Shape::Text(text) => texts.push(text.galley.text().into()),
            egui::Shape::Vec(shapes) => {
                for s in shapes {
                    gather(s, texts)
                }
            }
            _ => {}
        }
    }
    for _ in 0..3 {
        let out = ctx.run(Default::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| app.input_bar(ui));
        });
        texts.clear();
        for shape in out.shapes {
            gather(&shape.shape, &mut texts);
        }
    }
    assert!(!texts.iter().any(|text| text == "Send"));
    assert!(texts.iter().any(|text| text.contains("Enter to send")));
}
#[test]
fn skill_menu_keyboard_inserts_name_not_body_then_enter_can_send_request() {
    let mut app = tests::fixture();
    let home = std::env::temp_dir().join(feather_core::provider::gen_id("skills-egui"));
    let root = home.join("workspace");
    let skill = home.join("skills/demo");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&skill).unwrap();
    std::fs::write(
        skill.join("SKILL.md"),
        "---\nname: demo\ndescription: Documents and reports\n---\nSECRET_BODY",
    )
    .unwrap();
    app.home = home.clone();
    app.root = root;
    let ctx = app.egui_ctx.clone();
    theme::apply_theme(&ctx);
    app.input = "/skill".into();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let _ = ctx.run(Default::default(), |ctx| app.frame_ui(ctx));
        if app.skill_choices.loaded {
            break;
        }
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert_eq!(
        app.input_suggestions(),
        vec![("/skill:demo ".into(), "Documents and reports".into(), false)]
    );
    let enter = || egui::Event::Key {
        key: egui::Key::Enter,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::NONE,
    };
    let release = egui::Event::Key {
        key: egui::Key::Enter,
        physical_key: None,
        pressed: false,
        repeat: false,
        modifiers: egui::Modifiers::NONE,
    };
    let _ = ctx.run(
        egui::RawInput {
            events: vec![enter()],
            ..Default::default()
        },
        |ctx| app.frame_ui(ctx),
    );
    assert_eq!(app.input, "/skill:demo ");
    assert!(!app.running);
    assert!(app.rows.is_empty());
    assert!(app.input_suggestions().is_empty());
    let agent = tests::offline_agent();
    app.input_queue = agent.input_queue();
    app.running = true;
    let _ = ctx.run(
        egui::RawInput {
            events: vec![release, egui::Event::Text("write a report".into())],
            ..Default::default()
        },
        |ctx| app.frame_ui(ctx),
    );
    assert_eq!(app.input, "/skill:demo write a report");
    let _ = ctx.run(
        egui::RawInput {
            events: vec![enter()],
            ..Default::default()
        },
        |ctx| app.frame_ui(ctx),
    );
    assert!(
        app.input.is_empty(),
        "draft={:?}, focused={:?}, rows={:?}",
        app.input,
        ctx.memory(|m| m.focused()),
        app.rows.iter().map(|r| &r.text).collect::<Vec<_>>()
    );
    assert_eq!(app.queued_hint, 1);
    let mut agent = agent;
    app.rt.block_on(async {
        agent.prompt("start").await.unwrap();
    });
    assert!(agent
        .session()
        .messages()
        .iter()
        .any(|m| m.text().contains("SECRET_BODY") && m.text().contains("write a report")));
    drop(app);
    std::fs::remove_dir_all(home).unwrap();
}
#[test]
fn rewind_menu_can_read_saved_turns_while_agent_is_locked() {
    use feather_core::{
        agent::{AgentConfig, ProviderRef},
        provider::faux::{FauxProvider, FauxScript},
        session::Session,
        tool::ToolRegistry,
    };
    let home = std::env::temp_dir().join(feather_core::provider::gen_id("rewind-egui"));
    let root = home.join("workspace");
    std::fs::create_dir_all(&root).unwrap();
    let mut session = Session::new(&home, &root).unwrap();
    session
        .push(feather_core::message::Message::user_text("fix the parser"))
        .unwrap();
    session.commit_turn().unwrap();
    let mut app = tests::fixture();
    app.home = home.clone();
    app.root = root;
    let agent = Agent::new(
        AgentConfig {
            model: "fixture".into(),
            api: feather_core::message::Api::Faux,
            system_prompt: "synthetic".into(),
            max_tokens: 32,
            temperature: None,
            context_window: 10000,
            thinking_level: None,
            compaction: Default::default(),
            compaction_model: None,
            tools: Vec::new().into(),
        },
        session,
        Arc::new(ToolRegistry::new()),
        ProviderRef::Faux(FauxProvider::new(FauxScript::new().with_text("ok"))),
        None,
    );
    // Match the install path's immutable cloned snapshot handle, then hold the
    // same mutex a real response holds. The reader must still list durable turns.
    app.rewind_turns
        .attach(agent.session().snapshots().cloned());
    app.agent = Some(Arc::new(tokio::sync::Mutex::new(agent)));
    let shared = app.agent.clone().unwrap();
    let guard = shared.blocking_lock();
    app.running = true;
    let ctx = app.egui_ctx.clone();
    theme::apply_theme(&ctx);
    app.input = "/rewind".into();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let _ = ctx.run(Default::default(), |ctx| app.frame_ui(ctx));
        if app.rewind_turns.loaded {
            break;
        }
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert_eq!(
        app.input_suggestions(),
        vec![("/rewind 1".into(), "fix the parser".into(), true)]
    );
    drop(guard);
    app.running = false;
    let enter = egui::Event::Key {
        key: egui::Key::Enter,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::NONE,
    };
    let _ = ctx.run(
        egui::RawInput {
            events: vec![enter],
            ..Default::default()
        },
        |ctx| app.frame_ui(ctx),
    );
    assert!(app.input.is_empty());
    assert!(app
        .rows
        .iter()
        .any(|r| r.text.contains("Rewound LLM context to turn 1")));
    drop(shared);
    drop(app);
    std::fs::remove_dir_all(home).unwrap();
}
#[test]
fn rewind_keyboard_selects_labeled_turn_without_sending_it_to_model() {
    let mut app = tests::fixture();
    let ctx = app.egui_ctx.clone();
    theme::apply_theme(&ctx);
    app.input = "/rewind ".into();
    app.rewind_turns.loaded = true;
    app.rewind_turns.rows = vec![(3, "fix parser bug".into()), (2, "add tests".into())];
    let key = |key| egui::Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::NONE,
    };
    let _ = ctx.run(
        egui::RawInput {
            events: vec![key(egui::Key::ArrowDown), key(egui::Key::Enter)],
            ..Default::default()
        },
        |ctx| app.frame_ui(ctx),
    );
    assert!(!app.running);
    assert!(app.input.is_empty());
    assert!(app.rows.last().unwrap().text.contains("No active session"));
}
#[test]
fn rewind_dropdown_has_turn_labels_and_same_suggestion_contract() {
    let mut app = tests::fixture();
    app.rewind_turns.loaded = true;
    app.rewind_turns.rows = vec![(3, "fix parser bug".into()), (2, "add tests".into())];
    app.input = "/rewind ".into();
    let options = app.input_suggestions();
    assert_eq!(
        options[0],
        ("/rewind 3".into(), "fix parser bug".into(), true)
    );
    assert_eq!(options.len(), 2);
    app.input = "/rewind parser".into();
    assert_eq!(app.input_suggestions().len(), 1);
    app.input = "/".into();
    assert!(app
        .input_suggestions()
        .iter()
        .any(|(v, _, _)| v == "/rewind "));
}
#[test]
fn delete_removes_highlighted_proxy_but_not_while_typing_address() {
    let ctx = egui::Context::default();
    theme::apply_theme(&ctx);
    let mut catalog = feather::proxy::Catalog::default();
    let route =
        feather_core::provider::transport::Route::proxy("socks5h://127.0.0.1:12334").unwrap();
    catalog.entries.push(feather::proxy::CatalogEntry {
        endpoint: feather_core::provider::transport::Endpoint {
            route: route.clone(),
            protocol: feather_core::provider::transport::ProxyProtocol::Socks5,
            host: "127.0.0.1".into(),
            port: 12334,
        },
        last_seen: 0,
        active: true,
        manual: true,
    });
    let mut proxies = request_choices::Proxies::default();
    proxies.catalog = catalog;
    let mut picker = request_choices::Picker::Proxy {
        address: String::new(),
        cursor: 1,
        changed: false,
    };
    let mut open = true;
    for _ in 0..3 {
        let _ = ctx.run(Default::default(), |ctx| {
            request_choices::window(ctx, &mut picker, &proxies, &mut open);
        });
    }
    ctx.memory_mut(|m| {
        if let Some(id) = m.focused() {
            m.surrender_focus(id)
        }
    });
    let mut choice = None;
    let _ = ctx.run(
        egui::RawInput {
            events: vec![egui::Event::Key {
                key: egui::Key::Delete,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
            ..Default::default()
        },
        |ctx| {
            choice = request_choices::window(ctx, &mut picker, &proxies, &mut open);
        },
    );
    assert!(matches!(choice,Some(request_choices::Choice::Remove(ref found)) if found==&route));
}
