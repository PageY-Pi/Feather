use super::*;
use feather_core::message::{Api, ContentBlock, Message, ToolCall, Usage};

pub(crate) fn fixture() -> FeatherApp {
    let (ev_tx, ev_rx) = crossbeam_channel::unbounded();
    let (abort_tx, _) = tokio::sync::watch::channel(false);
    FeatherApp {
        rt: tokio::runtime::Runtime::new().unwrap().into(),
        agent: None,
        abort_tx,
        input_queue: Default::default(),
        ime_preedit_active: false,
        ev_tx,
        ev_rx,
        egui_ctx: Default::default(),
        settings: Default::default(),
        root: "fixture".into(),
        home: "fixture/home".into(),
        session_dir: "fixture/session".into(),
        rows: Vec::new(),
        history: Default::default(),
        input: String::new(),
        running: false,
        last_stop: None,
        queued_hint: 0,
        picker: None,
        kb: keybindings::Keybindings::load_from(std::path::Path::new("missing-test-bindings.json")),
        show_models: false,
        show_settings: false,
        show_login: false,
        show_thinking_picker: false,
        thinking_slider: 0,
        thinking_applied: 0,
        expand_tools: false,
        show_thinking: true,
        auth: Default::default(),
        api_key_drafts: Default::default(),
        auth_pending: Default::default(),
        auth_changed: false,
        catalog: None,
        metrics: Default::default(),
        last_usage: Default::default(),
        preview: true,
        model_filter: String::new(),
        auth_selected: 0,
        model_cursor: 0,
        model_manager: Default::default(),
        command_cursor: 0,
        capture_frames: 0,
        preview_timing: Default::default(),
        frame_pacer: Default::default(),
        rewind_turns: Default::default(),
        skill_choices: Default::default(),
        suggestions: Default::default(),
        renderer: renderer_choice::Backend::Cpu,
        renderer_picker: None,
        request_picker: None,
        proxies: Default::default(),
        exit: None,
        exit_ready: false,
    }
}

#[test]
fn ime_preedit_backspace_is_not_forwarded_to_text_edit() {
    let mut ime_preedit_active = false;
    let backspace = || egui::Event::Key {
        key: egui::Key::Backspace,
        physical_key: Some(egui::Key::Backspace),
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::NONE,
    };
    let mut input = egui::RawInput {
        events: vec![
            egui::Event::Ime(egui::ImeEvent::Enabled),
            egui::Event::Ime(egui::ImeEvent::Preedit("ceshi".into())),
            backspace(),
        ],
        ..Default::default()
    };

    suppress_ime_backspace(&mut input, &mut ime_preedit_active);
    assert!(ime_preedit_active);
    assert!(!input.events.iter().any(|event| {
        matches!(
            event,
            egui::Event::Key {
                key: egui::Key::Backspace,
                pressed: true,
                ..
            }
        )
    }));

    // The IME may report the next pre-edit value in a later frame. Until it
    // reports an empty value, an isolated Backspace is still IME-owned.
    input.events = vec![backspace()];
    suppress_ime_backspace(&mut input, &mut ime_preedit_active);
    assert!(input.events.is_empty());

    // Once the composition is empty, the next ordinary Backspace belongs to
    // TextEdit and can remove a committed character normally.
    input.events = vec![
        egui::Event::Ime(egui::ImeEvent::Preedit(String::new())),
        backspace(),
    ];
    suppress_ime_backspace(&mut input, &mut ime_preedit_active);
    assert!(!ime_preedit_active);
    assert!(!input.events.iter().any(|event| {
        matches!(
            event,
            egui::Event::Key {
                key: egui::Key::Backspace,
                pressed: true,
                ..
            }
        )
    }));
    input.events = vec![backspace()];
    suppress_ime_backspace(&mut input, &mut ime_preedit_active);
    assert_eq!(input.events, vec![backspace()]);
}

#[test]
fn ordinary_backspace_is_preserved_without_ime_preedit() {
    let mut ime_preedit_active = false;
    let mut input = egui::RawInput {
        events: vec![egui::Event::Key {
            key: egui::Key::Backspace,
            physical_key: Some(egui::Key::Backspace),
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }],
        ..Default::default()
    };
    suppress_ime_backspace(&mut input, &mut ime_preedit_active);
    assert_eq!(input.events.len(), 1);
    assert!(!ime_preedit_active);
}

fn call(id: &str) -> ToolCall {
    ToolCall {
        id: id.into(),
        name: "read".into(),
        arguments: serde_json::json!({"path":"中文.rs"}),
    }
}

#[test]
fn tool_completions_match_ids_not_latest_row() {
    let mut app = fixture();
    app.on_agent_event(AgentEvent::ToolStart { call: call("a") });
    app.on_agent_event(AgentEvent::ToolStart { call: call("b") });
    app.on_agent_event(AgentEvent::Notice("between".into()));
    app.on_agent_event(AgentEvent::ToolEnd {
        call: call("a"),
        text: "first".into(),
        is_error: false,
    });
    app.on_agent_event(AgentEvent::ToolEnd {
        call: call("b"),
        text: "second".into(),
        is_error: true,
    });
    assert_eq!(app.rows[0].tool_output.as_deref(), Some("first"));
    assert_eq!(app.rows[1].tool_output.as_deref(), Some("second"));
    assert!(!app.rows[0].tool_error);
    assert!(app.rows[1].tool_error);
    assert!(app.rows[0].elapsed.is_some());
    assert!(!app.rows[0].streaming);
}

#[test]
fn utf8_output_truncation_never_splits_a_character() {
    let text = "羽".repeat(10_000);
    let out = truncate_utf8(&text, 20_000);
    assert!(out.starts_with(&"羽".repeat(6666)));
    assert!(out.ends_with("… (truncated)"));
    assert_eq!(truncate_utf8("hello", 10), "hello");
}

#[test]
fn finalized_message_precedes_queued_user_delivery() {
    let mut app = fixture();
    app.on_agent_event(AgentEvent::TurnStart(1));
    app.on_agent_event(AgentEvent::TextDelta("partial".into()));
    app.running = true;
    app.input = "follow-up".into();
    app.submit();
    app.on_agent_event(AgentEvent::TextDelta(" more".into()));
    app.on_agent_event(AgentEvent::AssistantMessage(Message::assistant(
        vec![ContentBlock::Text {
            text: "complete".into(),
        }],
        "fixture",
        Api::Faux,
        Usage {
            output: 12,
            ..Default::default()
        },
        StopReason::Stop,
    )));
    assert_eq!(app.rows.len(), 1);
    assert_eq!(app.rows[0].text, "complete");
    assert!(!app.rows[0].streaming);
    app.on_agent_event(AgentEvent::AssistantMessage(Message::user_text(
        "follow-up",
    )));
    assert_eq!(app.rows.len(), 2);
}

#[test]
fn modal_enter_does_not_submit_composer() {
    let mut app = fixture();
    app.input = "keep draft".into();
    app.show_login = true;
    let ctx = app.egui_ctx.clone();
    app.handle_action("tui.input.submit", &ctx);
    assert_eq!(app.input, "keep draft");
    assert!(app.rows.is_empty());
}

#[test]
fn repeated_metric_completion_is_idempotent() {
    let mut m = metrics::StreamingMetrics::default();
    m.begin_run(0.0);
    m.begin_turn(0.0);
    m.record_delta("text", 1.0);
    m.finish_turn(10, 2.0);
    m.finish_turn(10, 100.0);
    assert_eq!(m.run_output_tokens(), 10);
    assert_eq!(m.run_generation_seconds(), 1.0);
    assert_eq!(m.label(100.0), "10.0 tok/s");
}

pub(crate) fn offline_agent() -> Agent {
    use feather_core::{
        agent::{AgentConfig, ProviderRef},
        provider::faux::{FauxProvider, FauxScript},
        session::Session,
        tool::ToolRegistry,
    };
    Agent::new(
        AgentConfig {
            model: "fixture".into(),
            api: Api::Faux,
            system_prompt: "Synthetic regression fixture".into(),
            max_tokens: 32,
            temperature: None,
            context_window: 10000,
            thinking_level: None,
            compaction: Default::default(),
            compaction_model: None,
            tools: Vec::new().into(),
        },
        Session::memory(std::path::Path::new("fixture"), "gui-submit-regression"),
        std::sync::Arc::new(ToolRegistry::new()),
        ProviderRef::Faux(FauxProvider::new(
            FauxScript::new()
                .with_text("first reply")
                .with_text("second reply"),
        )),
        None,
    )
}

#[test]
fn submit_inserts_user_before_first_assistant_and_resubmission() {
    let mut app = fixture();
    let agent = std::sync::Arc::new(tokio::sync::Mutex::new(offline_agent()));
    app.input_queue = app.rt.block_on(async { agent.lock().await.input_queue() });
    app.agent = Some(agent.clone());
    for text in ["first prompt", "second prompt"] {
        app.running = false;
        app.input = text.into();
        app.submit();
        app.rt.block_on(async {
            tokio::time::timeout(std::time::Duration::from_secs(3), async {
                loop {
                    let completed = {
                        let a = agent.lock().await;
                        let messages = a.session().messages();
                        messages
                            .last()
                            .is_some_and(|m| m.role == feather_core::message::Role::Assistant)
                            && messages.iter().any(|m| m.text() == text)
                    };
                    if completed {
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                }
            })
            .await
            .expect("submitted prompt completed");
        });
    }
    app.rt.block_on(async {
        let agent = agent.lock().await;
        let messages = agent.session().messages();
        assert_eq!(messages.len(), 4);
        assert_eq!(messages[0].text(), "first prompt");
        assert_eq!(messages[1].text(), "first reply");
        assert_eq!(messages[2].text(), "second prompt");
        assert_eq!(messages[3].text(), "second reply");
    });
}

#[test]
fn working_footer_has_animation_without_changing_composer_height() {
    let mut app = fixture();
    let ctx = app.egui_ctx.clone();
    theme::install_fonts(&ctx);
    theme::apply_theme(&ctx);
    let mut render = |running| {
        app.running = running;
        let mut height = 0.0;
        let output = ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1160.0, 900.0),
                )),
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    height = ui.scope(|ui| app.input_bar(ui)).response.rect.height();
                });
            },
        );
        (height, output)
    };
    render(false);
    let (idle_height, _) = render(false);
    let (working_height, output) = render(true);
    assert_eq!(
        idle_height, working_height,
        "composer must not jump when activity starts"
    );
    assert!(output.shapes.iter().any(|shape| matches!(&shape.shape,
        egui::Shape::Text(text) if text.galley.text() == "Proxy: Off")));
}

#[test]
fn submit_while_running_keeps_follow_up_in_shared_inbox() {
    let mut app = fixture();
    let agent = offline_agent();
    app.input_queue = agent.input_queue();
    app.running = true;
    app.input = "queued follow-up".into();
    app.submit();
    assert!(app.input.is_empty());
    assert_eq!(app.queued_hint, 1);
    let mut agent = agent;
    app.rt.block_on(async {
        agent.prompt("initial prompt").await.unwrap();
    });
    let messages = agent.session().messages();
    assert_eq!(messages.len(), 4);
    assert_eq!(messages[0].text(), "initial prompt");
    assert_eq!(messages[2].text(), "queued follow-up");
    assert_eq!(messages[3].text(), "second reply");
}

#[test]
fn reopening_reconstructs_rows_from_session_messages() {
    let messages = vec![
        Message::user_text("inspect login"),
        Message::assistant(
            vec![
                ContentBlock::Text {
                    text: "checking".into(),
                },
                ContentBlock::ToolCall(ToolCall {
                    id: "t1".into(),
                    name: "read".into(),
                    arguments: serde_json::json!({"path": "auth.rs"}),
                }),
            ],
            "fixture",
            Api::Faux,
            Usage::default(),
            StopReason::ToolUse,
        ),
        Message::tool_result("t1", "read", "auth body", false),
        Message::assistant(
            vec![ContentBlock::Text {
                text: "done".into(),
            }],
            "fixture",
            Api::Faux,
            Usage::default(),
            StopReason::Stop,
        ),
    ];
    let rows = rows_from_messages(&messages);
    assert_eq!(rows.len(), 4);
    assert_eq!(rows[0].kind, RowKind::User);
    assert_eq!(rows[0].text, "inspect login");
    assert_eq!(rows[1].kind, RowKind::Assistant);
    assert_eq!(rows[2].kind, RowKind::Tool);
    assert_eq!(rows[2].tool_id.as_deref(), Some("t1"));
    assert_eq!(rows[2].tool_args, "auth.rs");
    assert_eq!(rows[2].tool_output.as_deref(), Some("auth body"));
    assert!(!rows[2].tool_error);
    assert_eq!(rows[3].text, "done");
    assert!(
        !rows.iter().any(|row| row.streaming),
        "reopened rows are settled, not streaming"
    );
}

#[test]
fn tool_progress_streams_into_the_row_without_finalizing() {
    let mut app = fixture();
    app.on_agent_event(AgentEvent::ToolStart { call: call("a") });
    app.on_agent_event(AgentEvent::ToolProgress {
        call: call("a"),
        text: "line 1\nline 2".into(),
    });
    assert_eq!(app.rows[0].tool_output.as_deref(), Some("line 1\nline 2"));
    assert!(app.rows[0].streaming, "progress must not end the tool");
    app.on_agent_event(AgentEvent::ToolEnd {
        call: call("a"),
        text: "final".into(),
        is_error: false,
    });
    assert_eq!(app.rows[0].tool_output.as_deref(), Some("final"));
    assert!(!app.rows[0].streaming);
}

/// TEMPORARY: does the real panel layout move while the composer caret blinks,
/// moves, or text is typed? (*The* definition of the reported screen shake.)
#[test]
fn temp_shake_composer_layout_stability() {
    let mut app = fixture();
    let ctx = app.egui_ctx.clone();
    theme::install_fonts(&ctx);
    theme::apply_theme(&ctx);
    for i in 0..6 {
        app.rows
            .push(ChatRow::new(RowKind::User, format!("question {i}")));
        let mut a = ChatRow::new(RowKind::Assistant, "");
        a.text = "Reading the parser and the tokenizer first.\n".repeat(10);
        app.rows.push(a);
    }
    app.input = "hello world".to_string();

    let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1160.0, 820.0));
    let mut run = |app: &mut FeatherApp, time: f64, events: Vec<egui::Event>| {
        let mut bottom = egui::Rect::NOTHING;
        let mut inner = egui::Rect::NOTHING;
        let mut central = egui::Rect::NOTHING;
        let mut hint_y = f32::NAN;
        let mut caret = None;
        let output = ctx.run(
            egui::RawInput {
                screen_rect: Some(screen),
                time: Some(time),
                events,
                ..Default::default()
            },
            |ctx| {
                let r = egui::TopBottomPanel::bottom("feather_input")
                    .show_separator_line(false)
                    .resizable(false)
                    .frame(
                        egui::Frame::none()
                            .fill(crate::theme::PANEL)
                            .stroke(egui::Stroke::NONE)
                            .inner_margin(egui::Margin::symmetric(18.0, 8.0)),
                    )
                    .show(ctx, |ui| {
                        app.input_bar(ui);
                        inner = ui.min_rect();
                    });
                bottom = r.response.rect;
                let c = egui::CentralPanel::default()
                    .frame(egui::Frame::none().inner_margin(egui::Margin::symmetric(14.0, 8.0)))
                    .show(ctx, |ui| app.chat_panel(ui));
                central = c.response.rect;
            },
        );
        let mut chat_y = f32::NAN;
        let mut chat_rows = 0;
        for shape in &output.shapes {
            match &shape.shape {
                egui::Shape::Text(t) => {
                    if t.galley.text().starts_with("Enter to send") {
                        hint_y = t.pos.y;
                    }
                    if t.galley.text() == "Feather" {
                        chat_y = chat_y.max(t.pos.y);
                        chat_rows += 1;
                    }
                }
                egui::Shape::LineSegment { points, .. } => {
                    // egui paints the caret as a thin line.
                    if (points[1].x - points[0].x).abs() < 0.01
                        && (points[1].y - points[0].y).abs() > 4.0
                    {
                        caret = Some(points[0]);
                    }
                }
                _ => {}
            }
        }
        (
            output
                .viewport_output
                .values()
                .next()
                .map(|v| v.repaint_delay),
            bottom.height(),
            inner.height(),
            central.top(),
            hint_y,
            (caret, chat_y, chat_rows),
        )
    };

    let mut t = 0.0;
    for _ in 0..3 {
        t += 1.0 / 60.0;
        let _ = run(&mut app, t, vec![]);
    }
    let mut prev = run(&mut app, t, vec![]);
    eprintln!("idle  : {prev:?}");
    for frame in 0..4 {
        // Advance half a second: the caret blink phase flips.
        t += 0.5;
        let now = run(&mut app, t, vec![]);
        eprintln!(
            "blink{frame}: h={:.3} dh={:+.3} inner={:.3} central_top={:.3} hint_y={:.3} caret={:?}",
            now.1,
            now.1 - prev.1,
            now.2,
            now.3,
            now.4,
            now.5
        );
        prev = now;
    }

    let key = |k: egui::Key| egui::Event::Key {
        key: k,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: Default::default(),
    };
    for frame in 0..6 {
        t += 1.0 / 60.0;
        let now = run(&mut app, t, vec![key(egui::Key::ArrowLeft)]);
        eprintln!(
            "left {frame}: h={:.3} dh={:+.3} inner={:.3} central_top={:.3} hint_y={:.3} caret={:?}",
            now.1,
            now.1 - prev.1,
            now.2,
            now.3,
            now.4,
            now.5
        );
        prev = now;
    }
    for frame in 0..6 {
        t += 1.0 / 60.0;
        let now = run(&mut app, t, vec![key(egui::Key::ArrowRight)]);
        eprintln!(
            "right{frame}: h={:.3} dh={:+.3} inner={:.3} central_top={:.3} hint_y={:.3} caret={:?}",
            now.1,
            now.1 - prev.1,
            now.2,
            now.3,
            now.4,
            now.5
        );
        prev = now;
    }
    for frame in 0..8 {
        t += 1.0 / 60.0;
        let now = run(
            &mut app,
            t,
            vec![egui::Event::Text(
                if frame % 2 == 0 { "x" } else { "y" }.into(),
            )],
        );
        eprintln!(
            "type {frame}: h={:.3} dh={:+.3} inner={:.3} central_top={:.3} hint_y={:.3} caret={:?}",
            now.1,
            now.1 - prev.1,
            now.2,
            now.3,
            now.4,
            now.5
        );
        prev = now;
    }
}

/// TEMPORARY: with the caret parked at every column of a long input, does the
/// screen move? A caret-dependent height is exactly the reported shake.
#[test]
fn temp_shake_caret_positions() {
    for ppp in [1.0_f32, 1.25, 1.5] {
        let mut app = fixture();
        let ctx = app.egui_ctx.clone();
        theme::install_fonts(&ctx);
        theme::apply_theme(&ctx);
        ctx.set_pixels_per_point(ppp);
        for i in 0..6 {
            app.rows
                .push(ChatRow::new(RowKind::User, format!("question {i}")));
            let mut a = ChatRow::new(RowKind::Assistant, "");
            a.text = "Reading the parser and the tokenizer first.\n".repeat(10);
            app.rows.push(a);
        }
        // Long enough to wrap onto the composer's second and third visual rows.
        app.input = "the quick brown fox jumps over the lazy dog and keeps running".repeat(2);

        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1160.0, 820.0));
        let id = egui::Id::new("feather-composer");
        let mut run = |app: &mut FeatherApp, caret: usize, time: f64| {
            // Park the caret deterministically.
            ctx.memory_mut(|m| m.request_focus(id));
            if let Some(mut state) = egui::text_edit::TextEditState::load(&ctx, id) {
                state
                    .cursor
                    .set_char_range(Some(egui::text::CCursorRange::one(
                        egui::text::CCursor::new(caret),
                    )));
                state.store(&ctx, id);
            }
            let mut bottom = egui::Rect::NOTHING;
            let mut inner = egui::Rect::NOTHING;
            let mut central = egui::Rect::NOTHING;
            let output = ctx.run(
                egui::RawInput {
                    screen_rect: Some(screen),
                    time: Some(time),
                    ..Default::default()
                },
                |ctx| {
                    let r = egui::TopBottomPanel::bottom("feather_input")
                        .show_separator_line(false)
                        .resizable(false)
                        .frame(
                            egui::Frame::none()
                                .fill(crate::theme::PANEL)
                                .inner_margin(egui::Margin::symmetric(18.0, 8.0)),
                        )
                        .show(ctx, |ui| {
                            app.input_bar(ui);
                            inner = ui.min_rect();
                        });
                    bottom = r.response.rect;
                    let c = egui::CentralPanel::default()
                        .frame(egui::Frame::none().inner_margin(egui::Margin::symmetric(14.0, 8.0)))
                        .show(ctx, |ui| app.chat_panel(ui));
                    central = c.response.rect;
                },
            );
            let mut caret_y = f32::NAN;
            for shape in &output.shapes {
                if let egui::Shape::LineSegment { points, .. } = &shape.shape {
                    if (points[1].x - points[0].x).abs() < 0.01 {
                        caret_y = points[0].y;
                    }
                }
            }
            (bottom.height(), inner.height(), central.height(), caret_y)
        };

        let mut t = 0.0;
        let chars = app.input.chars().count();
        let mut prev = run(&mut app, chars, t);
        let mut worst: Option<(usize, f32, f32)> = None;
        for caret in 0..=chars {
            t += 1.0 / 60.0;
            let now = run(&mut app, caret, t);
            if now != prev {
                let delta_bottom = now.0 - prev.0;
                if worst.map_or(true, |(_, d, _)| delta_bottom.abs() > d.abs()) {
                    worst = Some((caret, delta_bottom, now.1 - prev.1));
                }
                prev = now;
            }
        }
        eprintln!(
            "ppp {ppp}: bottom_h={:.3} worst height change across carets: {worst:?}",
            prev.0
        );
    }
}

/// TEMPORARY: sweep window sizes and the running state, and report any geometry
/// that is not stable across idle repaints (blink), typing, and caret moves.
#[test]
fn temp_shake_geometry_sweep() {
    for (w, h) in [
        (1160.0_f32, 820.0_f32),
        (760.0, 520.0),
        (900.0, 600.0),
        (1160.0, 400.0),
    ] {
        for running in [false, true] {
            let mut app = fixture();
            let ctx = app.egui_ctx.clone();
            theme::install_fonts(&ctx);
            theme::apply_theme(&ctx);
            app.running = running;
            for i in 0..6 {
                app.rows
                    .push(ChatRow::new(RowKind::User, format!("question {i}")));
                let mut a = ChatRow::new(RowKind::Assistant, "");
                a.text = "Reading the parser and the tokenizer first.\n".repeat(10);
                if running && i == 5 {
                    a.streaming = true;
                }
                app.rows.push(a);
            }
            app.input = "hello".to_string();

            let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(w, h));
            let mut run = |app: &mut FeatherApp, time: f64, events: Vec<egui::Event>| {
                let mut bottom = egui::Rect::NOTHING;
                let mut central = egui::Rect::NOTHING;
                let output = ctx.run(
                    egui::RawInput {
                        screen_rect: Some(screen),
                        time: Some(time),
                        events,
                        ..Default::default()
                    },
                    |ctx| {
                        if app.running {
                            ctx.request_repaint_after(std::time::Duration::from_millis(100));
                        }
                        let r = egui::TopBottomPanel::bottom("feather_input")
                            .show_separator_line(false)
                            .resizable(false)
                            .frame(
                                egui::Frame::none()
                                    .fill(crate::theme::PANEL)
                                    .inner_margin(egui::Margin::symmetric(18.0, 8.0)),
                            )
                            .show(ctx, |ui| {
                                app.input_bar(ui);
                            });
                        bottom = r.response.rect;
                        let c = egui::CentralPanel::default()
                            .frame(
                                egui::Frame::none()
                                    .inner_margin(egui::Margin::symmetric(14.0, 8.0)),
                            )
                            .show(ctx, |ui| app.chat_panel(ui));
                        central = c.response.rect;
                    },
                );
                // A cheap fingerprint of everything painted this frame.
                let mut fingerprint = 0usize;
                let mut ys = Vec::new();
                for shape in &output.shapes {
                    match &shape.shape {
                        egui::Shape::Text(t) => {
                            fingerprint = fingerprint
                                .wrapping_mul(31)
                                .wrapping_add(t.galley.text().len())
                                .wrapping_add((t.pos.y * 64.0) as usize);
                            ys.push((
                                t.galley.text().chars().take(12).collect::<String>(),
                                t.pos.y,
                            ));
                        }
                        egui::Shape::LineSegment { points, .. } => {
                            fingerprint =
                                fingerprint.wrapping_add((points[0].y * 64.0) as usize + 7);
                        }
                        egui::Shape::Circle(_) => fingerprint = fingerprint.wrapping_add(3),
                        _ => {}
                    }
                }
                ys.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap());
                (
                    bottom.height(),
                    central.height(),
                    fingerprint,
                    ys.first().cloned(),
                    ys.last().cloned(),
                )
            };

            let mut t = 0.0;
            let mut prev = run(&mut app, t, vec![]);
            let base = prev.clone();
            let mut blink_moves = 0;
            let mut type_moves = 0;
            let mut caret_moves = 0;
            let key = |k: egui::Key| egui::Event::Key {
                key: k,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Default::default(),
            };
            // Idle repaints (caret blink cadence).
            for _ in 0..4 {
                t += 0.5;
                let now = run(&mut app, t, vec![]);
                let moved = now.1 != prev.1 || now.3 != prev.3 || now.4 != prev.4;
                if moved {
                    blink_moves += 1;
                    if blink_moves == 1 {
                        eprintln!("  IDLE MOVE {w}x{h} running={running}: {prev:?} -> {now:?}");
                    }
                }
                prev = now;
            }
            for _ in 0..6 {
                t += 1.0 / 60.0;
                let now = run(&mut app, t, vec![egui::Event::Text("x".into())]);
                let moved = now.1 != prev.1;
                if moved {
                    type_moves += 1;
                    if type_moves == 1 {
                        eprintln!(
                            "  TYPE MOVE {w}x{h} running={running}: dh={:+.3}",
                            now.1 - prev.1
                        );
                    }
                }
                prev = now;
            }
            for i in 0..6 {
                t += 1.0 / 60.0;
                let now = run(&mut app, t, vec![key(egui::Key::ArrowLeft)]);
                let moved = now.1 != prev.1;
                if moved {
                    caret_moves += 1;
                    if caret_moves == 1 && i == 0 {
                        eprintln!(
                            "  CARET MOVE {w}x{h} running={running}: dh={:+.3}",
                            now.1 - prev.1
                        );
                    }
                }
                prev = now;
            }
            eprintln!(
                "{w}x{h} running={running}: central_h={:.1} bottom_h={:.1} blink_moves={blink_moves} type_moves={type_moves} caret_moves={caret_moves}",
                base.1, base.0
            );
        }
    }
}

/// Canonical description of everything painted in a frame, so two frames can be
/// compared exactly (this is what "the screen moved" means).
/// Canonical description of everything painted in a frame as a multiset, so two
/// frames can be compared exactly (this is what "the screen moved" means).
fn temp_scene(output: &egui::FullOutput) -> std::collections::BTreeMap<String, usize> {
    let mut rows = std::collections::BTreeMap::new();
    for shape in &output.shapes {
        let key = match &shape.shape {
            egui::Shape::Text(t) => {
                format!("text {:>8.2} {:>8.2} {}", t.pos.x, t.pos.y, t.galley.text())
            }
            egui::Shape::Rect(r) => format!(
                "rect {:>8.2} {:>8.2} {:>8.2} {:>8.2}",
                r.rect.min.x,
                r.rect.min.y,
                r.rect.width(),
                r.rect.height()
            ),
            egui::Shape::Circle(c) => {
                format!(
                    "circ {:>8.2} {:>8.2} {:>6.2}",
                    c.center.x, c.center.y, c.radius
                )
            }
            egui::Shape::LineSegment { points, .. } => format!(
                "line {:>8.2} {:>8.2} {:>8.2} {:>8.2}",
                points[0].x, points[0].y, points[1].x, points[1].y
            ),
            egui::Shape::CubicBezier(b) => {
                format!("bezier {:>8.2} {:>8.2}", b.points[0].x, b.points[0].y)
            }
            egui::Shape::Path(p) => format!(
                "path {:>8.2} {:>8.2} {}",
                p.points.first().map(|p| p.x).unwrap_or_default(),
                p.points.first().map(|p| p.y).unwrap_or_default(),
                p.points.len()
            ),
            other => format!("other {other:?}"),
        };
        *rows.entry(key).or_insert(0) += 1;
    }
    rows
}

fn temp_first_diff(
    a: &std::collections::BTreeMap<String, usize>,
    b: &std::collections::BTreeMap<String, usize>,
) -> String {
    if a == b {
        return "identical".to_string();
    }
    let mut keys: Vec<&String> = a.keys().chain(b.keys()).collect();
    keys.sort();
    keys.dedup();
    let mut out = Vec::new();
    for key in keys {
        let (x, y) = (
            a.get(key).copied().unwrap_or(0),
            b.get(key).copied().unwrap_or(0),
        );
        if x != y {
            out.push(format!("{x}->{y} {key}"));
            if out.len() == 4 {
                break;
            }
        }
    }
    out.join(" | ")
}

/// TEMPORARY: drive the *real* frame layout and compare consecutive frames.
#[test]
fn temp_shake_real_frame_diff() {
    for pointer in [false, true] {
        let mut app = fixture();
        let ctx = app.egui_ctx.clone();
        theme::install_fonts(&ctx);
        theme::apply_theme(&ctx);
        for i in 0..8 {
            app.rows
                .push(ChatRow::new(RowKind::User, format!("question {i}")));
            let mut a = ChatRow::new(RowKind::Assistant, "");
            a.text =
                "Reading the parser and the tokenizer first.\n\nThe change is local.\n".repeat(6);
            app.rows.push(a);
        }
        app.input = "composing a message".to_string();

        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1160.0, 820.0));
        let mut run = |app: &mut FeatherApp, time: f64, events: Vec<egui::Event>| {
            let output = ctx.run(
                egui::RawInput {
                    screen_rect: Some(screen),
                    time: Some(time),
                    events,
                    ..Default::default()
                },
                |ctx| app.frame_ui(ctx),
            );
            temp_scene(&output)
        };

        let mut t = 0.0;
        // Warm up: egui panels only know their size from the previous pass.
        for _ in 0..3 {
            t += 1.0 / 60.0;
            let _ = run(&mut app, t, vec![]);
        }
        let mut last = run(&mut app, t, vec![]);
        let mut idle_diffs = 0;
        for _ in 0..5 {
            t += 0.5; // caret blink cadence
            let now = run(&mut app, t, vec![]);
            let diff = temp_first_diff(&last, &now);
            if diff != "identical" {
                idle_diffs += 1;
                if idle_diffs == 1 {
                    eprintln!("IDLE (pointer={pointer}) t={t}: {diff}");
                }
            }
            last = now;
        }
        let mut type_diffs = 0;
        for i in 0..4 {
            t += 1.0 / 60.0;
            let mut events = vec![egui::Event::Text("x".into())];
            if pointer {
                events.push(egui::Event::PointerMoved(egui::pos2(
                    600.0,
                    300.0 + i as f32,
                )));
            }
            let now = run(&mut app, t, events);
            let diff = temp_first_diff(&last, &now);
            if diff != "identical" {
                type_diffs += 1;
                if type_diffs == 1 {
                    eprintln!("TYPE (pointer={pointer}) t={t}: {diff}");
                }
            }
            last = now;
        }
        eprintln!("pointer={pointer}: idle_diffs={idle_diffs} type_diffs={type_diffs}");
    }
}

/// TEMPORARY: the realistic case — the model streams while the user types.
/// Does the transcript drift monotonically (normal) or oscillate (shake)?
#[test]
fn temp_shake_streaming_while_typing() {
    let mut app = fixture();
    let ctx = app.egui_ctx.clone();
    theme::install_fonts(&ctx);
    theme::apply_theme(&ctx);
    app.running = true;
    for i in 0..5 {
        app.rows
            .push(ChatRow::new(RowKind::User, format!("question {i}")));
        let mut a = ChatRow::new(RowKind::Assistant, "");
        a.text = "Reading the parser and the tokenizer first.\n".repeat(9);
        app.rows.push(a);
    }

    let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1160.0, 820.0));
    let mut run = |app: &mut FeatherApp, time: f64, events: Vec<egui::Event>| {
        let output = ctx.run(
            egui::RawInput {
                screen_rect: Some(screen),
                time: Some(time),
                events,
                ..Default::default()
            },
            |ctx| app.frame_ui(ctx),
        );
        let mut top = f32::NAN;
        let mut chat_bottom = f32::NAN;
        let mut caret_y = f32::NAN;
        for shape in &output.shapes {
            match &shape.shape {
                egui::Shape::Text(t) => {
                    if !t.galley.text().is_empty() && t.pos.y >= 60.0 && t.pos.y < 600.0 {
                        if t.pos.y < top || top.is_nan() {
                            top = t.pos.y;
                        }
                        if t.pos.y > chat_bottom || chat_bottom.is_nan() {
                            chat_bottom = t.pos.y;
                        }
                    }
                }
                egui::Shape::LineSegment { points, .. } => {
                    if (points[1].x - points[0].x).abs() < 0.01 {
                        caret_y = points[0].y;
                    }
                }
                _ => {}
            }
        }
        (top, chat_bottom, caret_y)
    };

    let mut t = 0.0;
    for _ in 0..3 {
        t += 1.0 / 60.0;
        let _ = run(&mut app, t, vec![]);
    }
    let mut prev = run(&mut app, t, vec![]);
    eprintln!("warm: {prev:?}");
    let mut rows = Vec::new();
    for frame in 0..24 {
        // ~200 tok/s: about 13 characters per 60 Hz frame.
        t += 1.0 / 60.0;
        app.on_agent_event(AgentEvent::TextDelta(
            "the change is local to the wrap cache ".into(),
        ));
        let mut events = Vec::new();
        if frame % 5 == 0 {
            events.push(egui::Event::Text("k".into()));
        }
        let now = run(&mut app, t, events);
        rows.push((
            now.0 - prev.0,
            now.1 - prev.1,
            now.2 - prev.2,
            app.rows.last().unwrap().text.len(),
        ));
        prev = now;
    }
    for (i, (dtop, dbottom, dcaret, len)) in rows.iter().enumerate() {
        eprintln!("frame {i:>2}: d_top={dtop:+.2} d_bottom={dbottom:+.2} caret={dcaret:+.2} text_len={len}");
    }
}

/// TEMPORARY: does the status bar (or the chat) jitter while the model streams?
#[test]
fn temp_shake_status_bar_jitter() {
    let mut app = fixture();
    let ctx = app.egui_ctx.clone();
    theme::install_fonts(&ctx);
    theme::apply_theme(&ctx);
    app.running = true;
    app.settings.model = "deepseek/deepseek-chat-v3-0324:free".to_string();
    app.on_agent_event(AgentEvent::TurnStart(0));
    for i in 0..5 {
        app.rows
            .push(ChatRow::new(RowKind::User, format!("question {i}")));
        let mut a = ChatRow::new(RowKind::Assistant, "");
        a.text = "Reading the parser and the tokenizer first.\n".repeat(9);
        app.rows.push(a);
    }

    let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1160.0, 820.0));
    let mut run = |app: &mut FeatherApp, time: f64| {
        let output = ctx.run(
            egui::RawInput {
                screen_rect: Some(screen),
                time: Some(time),
                ..Default::default()
            },
            |ctx| app.frame_ui(ctx),
        );
        let mut status = Vec::new();
        let mut bottom = egui::Rect::NOTHING;
        for shape in &output.shapes {
            if let egui::Shape::Text(t) = &shape.shape {
                if t.pos.y > 700.0 {
                    status.push(format!("{:.1}@{:.1}:{}", t.pos.x, t.pos.y, t.galley.text()));
                }
                if t.pos.y > 780.0 {
                    bottom = t.galley.rect;
                }
            }
        }
        status.sort();
        (status, bottom.min.y)
    };

    let mut t = 0.0;
    for _ in 0..3 {
        t += 1.0 / 60.0;
        let _ = run(&mut app, t);
    }
    let mut prev = run(&mut app, t);
    for frame in 0..12 {
        t += 1.0 / 60.0;
        // Stream: the live TPS label changes width as the rate changes.
        app.on_agent_event(AgentEvent::TextDelta(
            "the change is local to the wrap cache ".into(),
        ));
        if frame % 3 == 0 {
            // ...and occasionally a big burst, so the estimate swings.
            app.on_agent_event(AgentEvent::TextDelta("x".repeat(400)));
        }
        let now = run(&mut app, t);
        eprintln!(
            "frame {frame:>2} tps_label_jitter: {} status_bottom={:.1}",
            if now.0 == prev.0 { "stable" } else { "MOVED" },
            now.1
        );
        if now.0 != prev.0 {
            eprintln!("   prev: {:?}", prev.0);
            eprintln!("   now : {:?}", now.0);
        }
        prev = now;
    }
}

/// TEMPORARY: type one character at a time and watch every geometry that can
/// move the screen (panel heights, chat top).
#[test]
fn temp_shake_typing_geometry() {
    let mut app = fixture();
    let ctx = app.egui_ctx.clone();
    theme::install_fonts(&ctx);
    theme::apply_theme(&ctx);
    for i in 0..4 {
        app.rows
            .push(ChatRow::new(RowKind::User, format!("question {i}")));
        let mut a = ChatRow::new(RowKind::Assistant, "");
        a.text = "Reading the parser and the tokenizer first.\n".repeat(8);
        app.rows.push(a);
    }
    app.input.clear();

    let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1160.0, 820.0));
    let id = egui::Id::new("feather-composer");
    let mut run = |app: &mut FeatherApp, time: f64, caret: usize| {
        ctx.memory_mut(|m| m.request_focus(id));
        if let Some(mut state) = egui::text_edit::TextEditState::load(&ctx, id) {
            state
                .cursor
                .set_char_range(Some(egui::text::CCursorRange::one(
                    egui::text::CCursor::new(caret),
                )));
            state.store(&ctx, id);
        }
        let mut bottom = egui::Rect::NOTHING;
        let mut central = egui::Rect::NOTHING;
        let output = ctx.run(
            egui::RawInput {
                screen_rect: Some(screen),
                time: Some(time),
                ..Default::default()
            },
            |ctx| {
                let r = egui::TopBottomPanel::bottom("feather_input")
                    .show_separator_line(false)
                    .resizable(false)
                    .frame(
                        egui::Frame::none()
                            .fill(crate::theme::PANEL)
                            .inner_margin(egui::Margin::symmetric(18.0, 8.0)),
                    )
                    .show(ctx, |ui| {
                        app.input_bar(ui);
                    });
                bottom = r.response.rect;
                let c = egui::CentralPanel::default()
                    .frame(egui::Frame::none().inner_margin(egui::Margin::symmetric(14.0, 8.0)))
                    .show(ctx, |ui| app.chat_panel(ui));
                central = c.response.rect;
            },
        );
        let mut chat_top = f32::NAN;
        for shape in &output.shapes {
            if let egui::Shape::Text(t) = &shape.shape {
                if t.pos.y >= 60.0 && t.pos.y < 600.0 && (chat_top.is_nan() || t.pos.y < chat_top) {
                    chat_top = t.pos.y;
                }
            }
        }
        (bottom.height(), central.height(), chat_top)
    };

    // Type a long paragraph, one character at a time: the composer grows as it
    // wraps, and a trailing space at the wrap boundary is the classic trigger.
    let text =
        "the quick brown fox jumps over the lazy dog and then keeps on running past the fence "
            .repeat(4);
    let mut t = 0.0;
    let mut prev = run(&mut app, t, 0);
    let mut flips = 0;
    for (i, ch) in text.chars().enumerate() {
        app.input.push(ch);
        t += 1.0 / 60.0;
        let caret = app.input.chars().count();
        let now = run(&mut app, t, caret);
        let dh = now.0 - prev.0;
        let dchat = now.0 - prev.0;
        if dh.abs() > 0.01 {
            flips += 1;
            eprintln!(
                "{:>3} {:?} -> h={:.2} dh={:+.2} central={:.2} chat_top={:.2} (d_top={:+.2})",
                i,
                ch,
                now.0,
                dh,
                now.1,
                now.2,
                now.2 - prev.2
            );
        }
        let _ = dchat;
        prev = now;
    }
    eprintln!("height changes while typing: {flips}");
}

/// TEMPORARY: how expensive is one real frame as a session grows?
#[test]
fn temp_shake_frame_cost_by_session_size() {
    for rows in [20usize, 100, 300] {
        let mut app = fixture();
        let ctx = app.egui_ctx.clone();
        theme::install_fonts(&ctx);
        theme::apply_theme(&ctx);
        for i in 0..rows {
            app.rows
                .push(ChatRow::new(RowKind::User, format!("question {i}")));
            let mut a = ChatRow::new(RowKind::Assistant, "");
            a.text = format!(
                "## Plan {i}\n\nReading the parser and the tokenizer first, then the wrap cache.\n\n```rust\nlet x = {i};\n```\n\n- one\n- two\n\nAll good.\n"
            );
            app.rows.push(a);
        }
        app.input = "composing".to_string();

        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1160.0, 820.0));
        let mut run = |app: &mut FeatherApp, time: f64, events: Vec<egui::Event>| {
            ctx.run(
                egui::RawInput {
                    screen_rect: Some(screen),
                    time: Some(time),
                    events,
                    ..Default::default()
                },
                |ctx| app.frame_ui(ctx),
            )
        };
        let mut t = 0.0;
        for _ in 0..3 {
            t += 1.0 / 60.0;
            let _ = run(&mut app, t, vec![]);
        }
        let t0 = std::time::Instant::now();
        for _ in 0..20 {
            t += 1.0 / 60.0;
            let _ = run(&mut app, t, vec![]);
        }
        let idle = t0.elapsed() / 20;
        let t0 = std::time::Instant::now();
        for i in 0..20 {
            t += 1.0 / 60.0;
            let _ = run(&mut app, t, vec![egui::Event::Text("x".into())]);
            let _ = i;
        }
        let typing = t0.elapsed() / 20;
        eprintln!(
            "{} rows ({} chat rows): idle {idle:?}/frame, typing {typing:?}/frame",
            rows,
            app.rows.len()
        );
    }
}

/// TEMPORARY: a long message in the composer (taller than the window) — does the
/// layout oscillate instead of settling?
#[test]
fn temp_shake_long_composer() {
    let mut app = fixture();
    let ctx = app.egui_ctx.clone();
    theme::install_fonts(&ctx);
    theme::apply_theme(&ctx);
    for i in 0..4 {
        app.rows
            .push(ChatRow::new(RowKind::User, format!("question {i}")));
        let mut a = ChatRow::new(RowKind::Assistant, "");
        a.text = "Reading the parser and the tokenizer first.\n".repeat(8);
        app.rows.push(a);
    }
    // A long pasted prompt: way taller than the available window height.
    app.input =
        "the quick brown fox jumps over the lazy dog and keeps running past the fence\n".repeat(30);

    let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1160.0, 820.0));
    let id = egui::Id::new("feather-composer");
    let mut run = |app: &mut FeatherApp, time: f64, caret: usize| {
        ctx.memory_mut(|m| m.request_focus(id));
        if let Some(mut state) = egui::text_edit::TextEditState::load(&ctx, id) {
            state
                .cursor
                .set_char_range(Some(egui::text::CCursorRange::one(
                    egui::text::CCursor::new(caret),
                )));
            state.store(&ctx, id);
        }
        let mut bottom = egui::Rect::NOTHING;
        let mut central = egui::Rect::NOTHING;
        let output = ctx.run(
            egui::RawInput {
                screen_rect: Some(screen),
                time: Some(time),
                ..Default::default()
            },
            |ctx| {
                let r = egui::TopBottomPanel::bottom("feather_input")
                    .show_separator_line(false)
                    .resizable(false)
                    .frame(
                        egui::Frame::none()
                            .fill(crate::theme::PANEL)
                            .inner_margin(egui::Margin::symmetric(18.0, 8.0)),
                    )
                    .show(ctx, |ui| {
                        app.input_bar(ui);
                    });
                bottom = r.response.rect;
                let c = egui::CentralPanel::default()
                    .frame(egui::Frame::none().inner_margin(egui::Margin::symmetric(14.0, 8.0)))
                    .show(ctx, |ui| app.chat_panel(ui));
                central = c.response.rect;
            },
        );
        let mut chat_top = f32::NAN;
        let mut composer_top = f32::NAN;
        for shape in &output.shapes {
            if let egui::Shape::Text(t) = &shape.shape {
                if t.pos.y >= 60.0 && t.pos.y < 600.0 && (chat_top.is_nan() || t.pos.y < chat_top) {
                    chat_top = t.pos.y;
                }
                if t.galley.text().starts_with("the quick brown fox") {
                    composer_top = composer_top.min(t.pos.y);
                }
            }
        }
        (bottom.height(), central.height(), chat_top, composer_top)
    };

    let chars = app.input.chars().count();
    let mut t = 0.0;
    for _ in 0..6 {
        t += 1.0 / 60.0;
        let _ = run(&mut app, t, chars);
    }
    let mut prev = run(&mut app, t, chars);
    eprintln!("settled: {prev:?}");
    for frame in 0..8 {
        t += 1.0 / 60.0;
        let now = run(&mut app, t, chars);
        eprintln!(
            "idle {frame}: h={:.2} ({:+.2}) central={:.2} ({:+.2}) chat_top={:.2} ({:+.2})",
            now.0,
            now.0 - prev.0,
            now.1,
            now.1 - prev.1,
            now.2,
            now.2 - prev.2
        );
        prev = now;
    }
    // Move the caret around: the caret row changes, the panel must not.
    for frame in 0..8 {
        t += 1.0 / 60.0;
        let caret = chars - frame * 7;
        let now = run(&mut app, t, caret);
        eprintln!(
            "caret {caret:>5}: h={:.2} ({:+.2}) central={:.2} ({:+.2}) composer_top={:.2} ({:+.2})",
            now.0,
            now.0 - prev.0,
            now.1,
            now.1 - prev.1,
            now.3,
            now.3 - prev.3
        );
        prev = now;
    }
}

/// TEMPORARY: with the composer squeezed by a top panel, does moving the caret
/// scroll the *chat* (egui publishes the caret's scroll target frame-globally)?
#[test]
fn temp_shake_caret_scroll_target_leak() {
    let mut app = fixture();
    let ctx = app.egui_ctx.clone();
    theme::install_fonts(&ctx);
    theme::apply_theme(&ctx);
    app.running = false;
    for i in 0..6 {
        app.rows
            .push(ChatRow::new(RowKind::User, format!("question {i}")));
        let mut a = ChatRow::new(RowKind::Assistant, "");
        a.text = "Reading the parser and the tokenizer first.\n".repeat(8);
        app.rows.push(a);
    }
    app.input = "the quick brown fox jumps over the lazy dog\n".repeat(14);

    let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1160.0, 820.0));
    let id = egui::Id::new("feather-composer");
    let mut run = |app: &mut FeatherApp, time: f64, caret: usize| {
        ctx.memory_mut(|m| m.request_focus(id));
        if let Some(mut state) = egui::text_edit::TextEditState::load(&ctx, id) {
            state
                .cursor
                .set_char_range(Some(egui::text::CCursorRange::one(
                    egui::text::CCursor::new(caret),
                )));
            state.store(&ctx, id);
        }
        let mut bottom = egui::Rect::NOTHING;
        let mut central = egui::Rect::NOTHING;
        let output = ctx.run(
            egui::RawInput {
                screen_rect: Some(screen),
                time: Some(time),
                ..Default::default()
            },
            |ctx| {
                // A top panel like the real app's, so the composer gets squeezed.
                egui::TopBottomPanel::top("feather_top")
                    .show_separator_line(false)
                    .resizable(false)
                    .frame(egui::Frame::none().fill(crate::theme::PANEL))
                    .show(ctx, |ui| {
                        ui.allocate_space(egui::vec2(ui.available_width(), 41.0));
                    });
                let r = egui::TopBottomPanel::bottom("feather_input")
                    .show_separator_line(false)
                    .resizable(false)
                    .frame(
                        egui::Frame::none()
                            .fill(crate::theme::PANEL)
                            .inner_margin(egui::Margin::symmetric(18.0, 8.0)),
                    )
                    .show(ctx, |ui| {
                        app.input_bar(ui);
                    });
                bottom = r.response.rect;
                let c = egui::CentralPanel::default()
                    .frame(egui::Frame::none().inner_margin(egui::Margin::symmetric(14.0, 8.0)))
                    .show(ctx, |ui| app.chat_panel(ui));
                central = c.response.rect;
            },
        );
        let mut chat_texts: Vec<(f32, String)> = Vec::new();
        let mut caret_y = f32::NAN;
        for shape in &output.shapes {
            match &shape.shape {
                egui::Shape::Text(t) => {
                    if t.pos.y < central.max.y {
                        chat_texts.push((t.pos.y, t.galley.text().chars().take(18).collect()));
                    }
                }
                egui::Shape::LineSegment { points, .. } => {
                    if (points[1].x - points[0].x).abs() < 0.01 {
                        caret_y = points[0].y;
                    }
                }
                _ => {}
            }
        }
        chat_texts.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
        (
            bottom.height(),
            central.height(),
            chat_texts.first().cloned(),
            chat_texts.last().cloned(),
            caret_y,
        )
    };

    let chars = app.input.chars().count();
    let mut t = 0.0;
    for _ in 0..6 {
        t += 1.0 / 60.0;
        let _ = run(&mut app, t, chars);
    }
    let mut prev = run(&mut app, t, chars);
    eprintln!(
        "settled: bottom={:.1} central={:.1} first={:?} last={:?} caret_y={:.1}",
        prev.0, prev.1, prev.2, prev.3, prev.4
    );
    for frame in 0..10 {
        t += 1.0 / 60.0;
        let caret = chars - frame * 9;
        let now = run(&mut app, t, caret);
        eprintln!(
            "caret {caret:>4}: bottom={:.1} central={:.1} caret_y={:.1} first_y={:.1} last_y={:.1}",
            now.0,
            now.1,
            now.4,
            now.2.as_ref().map(|c| c.0).unwrap_or(f32::NAN),
            now.3.as_ref().map(|c| c.0).unwrap_or(f32::NAN)
        );
        prev = now;
    }
}

/// TEMPORARY: a composer taller than the window (the panel clamps it) makes the
/// TextEdit "not fully visible", so egui publishes the caret as a scroll target.
/// The chat's ScrollArea is rendered later and would eat it.
#[test]
fn temp_shake_composer_caret_leaks_into_chat() {
    let mut app = fixture();
    let ctx = app.egui_ctx.clone();
    theme::install_fonts(&ctx);
    theme::apply_theme(&ctx);
    for i in 0..9 {
        app.rows
            .push(ChatRow::new(RowKind::User, format!("question {i}")));
        let mut a = ChatRow::new(RowKind::Assistant, "");
        a.text = "Reading the parser and the tokenizer first.\n".repeat(8);
        app.rows.push(a);
    }
    // 40 lines: taller than the window, so the bottom panel clamps.
    app.input = "the quick brown fox jumps over the lazy dog\n".repeat(40);

    let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1160.0, 820.0));
    let id = egui::Id::new("feather-composer");
    let mut run = |app: &mut FeatherApp, time: f64, caret: usize, events: Vec<egui::Event>| {
        ctx.memory_mut(|m| m.request_focus(id));
        if let Some(mut state) = egui::text_edit::TextEditState::load(&ctx, id) {
            state
                .cursor
                .set_char_range(Some(egui::text::CCursorRange::one(
                    egui::text::CCursor::new(caret),
                )));
            state.store(&ctx, id);
        }
        let mut bottom = egui::Rect::NOTHING;
        let mut central = egui::Rect::NOTHING;
        let output = ctx.run(
            egui::RawInput {
                screen_rect: Some(screen),
                time: Some(time),
                events,
                ..Default::default()
            },
            |ctx| {
                let r = egui::TopBottomPanel::bottom("feather_input")
                    .show_separator_line(false)
                    .resizable(false)
                    .frame(
                        egui::Frame::none()
                            .fill(crate::theme::PANEL)
                            .inner_margin(egui::Margin::symmetric(18.0, 8.0)),
                    )
                    .show(ctx, |ui| {
                        app.input_bar(ui);
                    });
                bottom = r.response.rect;
                let c = egui::CentralPanel::default()
                    .frame(egui::Frame::none().inner_margin(egui::Margin::symmetric(14.0, 8.0)))
                    .show(ctx, |ui| app.chat_panel(ui));
                central = c.response.rect;
            },
        );
        let mut chat_ys: Vec<f32> = Vec::new();
        for shape in &output.shapes {
            if let egui::Shape::Text(t) = &shape.shape {
                if t.pos.y >= central.min.y - 40.0 && t.pos.y <= central.max.y + 40.0 {
                    chat_ys.push(t.pos.y);
                }
            }
        }
        chat_ys.sort_by(|a, b| a.partial_cmp(b).unwrap());
        (
            bottom.height(),
            central.height(),
            chat_ys.first().copied().unwrap_or(f32::NAN),
            chat_ys.last().copied().unwrap_or(f32::NAN),
        )
    };

    let chars = app.input.chars().count();
    let mut t = 0.0;
    for _ in 0..6 {
        t += 1.0 / 60.0;
        let _ = run(&mut app, t, chars, vec![]);
    }
    let mut prev = run(&mut app, t, chars, vec![]);
    eprintln!(
        "settled: bottom={:.1} central={:.1} first_y={:.1} last_y={:.1}",
        prev.0, prev.1, prev.2, prev.3
    );

    // Scroll the chat up, like a user reading back through the transcript.
    for _ in 0..3 {
        t += 1.0 / 60.0;
        let now = run(
            &mut app,
            t,
            chars,
            vec![
                egui::Event::PointerMoved(egui::pos2(600.0, 300.0)),
                egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Line,
                    delta: egui::vec2(0.0, -6.0),
                    modifiers: Default::default(),
                },
            ],
        );
        eprintln!("scrolled: first_y={:.1} last_y={:.1}", now.2, now.3);
        prev = now;
    }

    // Now type a single character.
    for i in 0..4 {
        t += 1.0 / 60.0;
        app.input.push('x');
        let now = run(
            &mut app,
            t,
            chars + i + 1,
            vec![egui::Event::Text("x".into())],
        );
        eprintln!(
            "typed {i}: bottom={:.1} central={:.1} first_y={:.1} ({:+.1}) last_y={:.1} ({:+.1})",
            now.0,
            now.1,
            now.2,
            now.2 - prev.2,
            now.3,
            now.3 - prev.3
        );
        prev = now;
    }

    // And move the caret left without changing the text.
    for i in 0..4 {
        t += 1.0 / 60.0;
        let caret = chars - i * 3;
        let now = run(&mut app, t, caret, vec![]);
        eprintln!(
            "caret {caret:>4}: first_y={:.1} ({:+.1}) last_y={:.1} ({:+.1})",
            now.2,
            now.2 - prev.2,
            now.3,
            now.3 - prev.3
        );
        prev = now;
    }
}

/// TEMPORARY: who keeps asking for repaints while the user is just typing?
#[test]
fn temp_shake_repaint_causes() {
    let mut app = fixture();
    let ctx = app.egui_ctx.clone();
    theme::install_fonts(&ctx);
    theme::apply_theme(&ctx);
    for i in 0..6 {
        app.rows
            .push(ChatRow::new(RowKind::User, format!("question {i}")));
        let mut a = ChatRow::new(RowKind::Assistant, "");
        a.text = "Reading the parser and the tokenizer first.\n".repeat(8);
        app.rows.push(a);
    }
    app.input = "composing".to_string();

    let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1160.0, 820.0));
    let mut run = |app: &mut FeatherApp, time: f64, events: Vec<egui::Event>, label: &str| {
        let output = ctx.run(
            egui::RawInput {
                screen_rect: Some(screen),
                time: Some(time),
                events,
                ..Default::default()
            },
            |ctx| app.frame_ui(ctx),
        );
        let delay = output
            .viewport_output
            .values()
            .next()
            .map(|v| v.repaint_delay)
            .unwrap_or(std::time::Duration::MAX);
        let causes: Vec<String> = ctx
            .repaint_causes()
            .into_iter()
            .map(|c| format!("{}:{} ({})", c.file, c.line, c.reason))
            .collect();
        eprintln!("{label}: repaint_delay={delay:?} causes={causes:?}");
    };

    let mut t = 0.0;
    for _ in 0..3 {
        t += 1.0 / 60.0;
        run(&mut app, t, vec![], "warm");
    }
    for _ in 0..3 {
        t += 0.5; // blink cadence, idle
        run(&mut app, t, vec![], "idle");
    }
    for _ in 0..3 {
        t += 1.0 / 60.0;
        run(&mut app, t, vec![egui::Event::Text("x".into())], "type");
    }
    app.running = true;
    for _ in 0..3 {
        t += 1.0 / 60.0;
        run(&mut app, t, vec![], "running");
    }
}
