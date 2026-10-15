use super::*;
#[test]
fn footer_separator_gaps_are_identical_across_left_and_right_groups() {
    for width in [510.0, 950.0, 1400.0] {
        let mut app = tests::fixture();
        app.metrics.record_delta("hello world", 1.0);
        let ctx = app.egui_ctx.clone();
        theme::apply_theme(&ctx);
        let mut rows = Vec::new();
        for _ in 0..3 {
            let out = ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(width, 150.0),
                    )),
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| app.status_bar(ui));
                },
            );
            rows.clear();
            for job in &out.shapes {
                texts(&job.shape, &mut rows);
            }
        }
        rows.sort_by_key(|(_, rect)| (rect.center().x * 100.0) as i64);
        let mut gaps = Vec::new();
        for (index, (label, rect)) in rows.iter().enumerate() {
            if label != "·" {
                continue;
            }
            let previous = rows
                .get(index.saturating_sub(1))
                .map(|(_, r)| r)
                .expect("separator has a left neighbour");
            let next = rows
                .get(index + 1)
                .map(|(_, r)| r)
                .expect("separator has a right neighbour");
            gaps.push(rect.left() - previous.right());
            gaps.push(next.left() - rect.right());
        }
        assert!(gaps.len() >= 8, "expected four separators, found {gaps:?}");
        let smallest = gaps.iter().copied().fold(f32::INFINITY, f32::min);
        let largest = gaps.iter().copied().fold(f32::NEG_INFINITY, f32::max);
        assert!(
            largest - smallest < 1.6,
            "separator gaps must be uniform at width {width}: {gaps:?}"
        );
    }
}

#[test]
fn minimized_transient_surface_does_not_layout_panels_or_lose_close() {
    let mut app = tests::fixture();
    let ctx = app.egui_ctx.clone();
    theme::apply_theme(&ctx);
    for size in [
        egui::vec2(199.0, 34.0),
        egui::vec2(0.0, 0.0),
        egui::vec2(950.0, 820.0),
    ] {
        let _ = ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
                ..Default::default()
            },
            |ctx| app.frame_ui(ctx),
        );
    }
    app.request_close();
    let out = ctx.run(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(199.0, 34.0),
            )),
            ..Default::default()
        },
        |ctx| app.update_ui(ctx),
    );
    assert!(app.exit_ready);
    assert!(out.viewport_output[&egui::ViewportId::ROOT]
        .commands
        .contains(&egui::ViewportCommand::Close));
}
fn texts(shape: &egui::Shape, out: &mut Vec<(String, egui::Rect)>) {
    match shape {
        egui::Shape::Text(text) => {
            out.push((
                text.galley.text().to_owned(),
                text.galley.rect.translate(text.pos.to_vec2()),
            ));
        }
        egui::Shape::Vec(shapes) => {
            for shape in shapes {
                texts(shape, out);
            }
        }
        _ => {}
    }
}
#[test]
fn renderer_footer_is_text_only_and_status_stays_on_the_right() {
    for width in [510.0, 950.0] {
        let mut app = tests::fixture();
        let ctx = app.egui_ctx.clone();
        theme::apply_theme(&ctx);
        let mut found = Vec::new();
        for _ in 0..3 {
            let out = ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(width, 150.0),
                    )),
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| app.status_bar(ui));
                },
            );
            found.clear();
            for job in &out.shapes {
                texts(&job.shape, &mut found);
            }
        }
        let rect = |s: &str| {
            found
                .iter()
                .find(|(t, _)| t == s)
                .unwrap_or_else(|| panic!("missing {s}: {found:?}"))
                .1
        };
        assert!(
            !found
                .iter()
                .any(|(text, _)| text == "CPU" || text == "OpenGL"),
            "no permanent dropdown/options in the footer"
        );
        assert!(rect("Render: CPU").center().x > width * 0.6);
        assert!(rect("Proxy: Off").center().x > rect("Render: CPU").center().x);
        assert!((rect("Proxy: Off").center().y - rect("Render: CPU").center().y).abs() < 8.0);
        let session = found
            .iter()
            .find(|(text, _)| text.starts_with("fixture") || text == "session")
            .unwrap()
            .1;
        let model = found
            .iter()
            .find(|(text, _)| text.starts_with("claude"))
            .unwrap()
            .1;
        let effort = found
            .iter()
            .find(|(text, _)| text.starts_with("think") || text.starts_with("effort"))
            .unwrap()
            .1;
        for label in [session, model, effort] {
            assert!(
                (label.center().y - rect("Render: CPU").center().y).abs() < 1.0,
                "all controls share the same baseline"
            );
        }
        assert!(
            found.iter().all(|(_, rect)| rect.center().y < 40.0),
            "no second renderer/proxy row"
        );

        assert!(
            rect("Proxy: Off").right() <= width,
            "right-aligned footer must not bleed outside viewport"
        );
        assert!(ctx.used_rect().width() <= width);
    }
}
#[test]
fn header_close_signals_abort_and_rejects_new_prompt_without_blocking() {
    let mut app = tests::fixture();
    let mut aborted = app.abort_tx.subscribe();
    app.running = true;
    app.input = "unsent draft".into();
    app.request_close();
    assert!(*aborted.borrow_and_update());
    app.submit();
    assert_eq!(app.input, "unsent draft");
    let ctx = app.egui_ctx.clone();
    let out = ctx.run(Default::default(), |ctx| app.poll_exit(ctx));
    assert!(!app.exit_ready);
    assert!(out.viewport_output[&egui::ViewportId::ROOT]
        .commands
        .contains(&egui::ViewportCommand::CancelClose));
    app.running = false;
    let out = ctx.run(Default::default(), |ctx| app.poll_exit(ctx));
    assert!(app.exit_ready);
    assert!(out.viewport_output[&egui::ViewportId::ROOT]
        .commands
        .contains(&egui::ViewportCommand::Close));
}
#[test]
fn active_run_disables_renderer_restarts_and_label_tracks_actual_backend() {
    let mut app = tests::fixture();
    app.running = true;
    app.switch_renderer(renderer_choice::Backend::OpenGl);
    assert!(app.exit.is_none());
    app.renderer = renderer_choice::Backend::OpenGl;
    let ctx = app.egui_ctx.clone();
    let out = ctx.run(Default::default(), |ctx| {
        egui::CentralPanel::default().show(ctx, |ui| app.status_bar(ui));
    });
    let mut found = Vec::new();
    for job in &out.shapes {
        texts(&job.shape, &mut found);
    }
    assert!(found.iter().any(|(text, _)| text == "Render: GPU"));
}

#[test]
fn close_during_durable_tool_run_commits_cancellation_and_releases_session() {
    use feather_core::{
        agent::{AgentConfig, ProviderRef},
        message::{Api, StopReason},
        provider::faux::{FauxProvider, FauxScript},
        tool::{AgentTool, BoxFut, ToolOutcome, ToolRegistry},
        Session,
    };
    struct WaitForCancel;
    impl AgentTool for WaitForCancel {
        fn name(&self) -> &'static str {
            "wait_for_close"
        }
        fn description(&self) -> &'static str {
            "synthetic cancellation gate"
        }
        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({"type":"object"})
        }
        fn execute<'a>(
            &'a self,
            _: serde_json::Value,
            mut abort: tokio::sync::watch::Receiver<bool>,
        ) -> BoxFut<'a> {
            Box::pin(async move {
                while !*abort.borrow() {
                    if abort.changed().await.is_err() {
                        break;
                    }
                }
                ToolOutcome::err("cancelled by close")
            })
        }
    }
    let home = std::env::temp_dir().join(feather_core::provider::gen_id("close-durable-test"));
    let root = home.join("workspace");
    std::fs::create_dir_all(&root).unwrap();
    let session = Session::new(&home, &root).unwrap();
    let dir = session.dir().to_path_buf();
    let mut tools = ToolRegistry::new();
    tools.add(Arc::new(WaitForCancel));
    let agent = Agent::new(
        AgentConfig {
            model: "synthetic".into(),
            api: Api::Faux,
            system_prompt: "synthetic".into(),
            max_tokens: 1000,
            temperature: None,
            context_window: 200_000,
            thinking_level: None,
            compaction: Default::default(),
            compaction_model: None,
            tools: tools.defs().into(),
        },
        session,
        Arc::new(tools),
        ProviderRef::Faux(FauxProvider::new(FauxScript::new().with_text_then(
            "partial answer",
            "wait_for_close",
            serde_json::json!({}),
        ))),
        None,
    );
    let mut app = tests::fixture();
    app.home = home.clone();
    app.root = root;
    app.session_dir = dir.clone();
    app.input_queue = agent.input_queue();
    app.abort_tx = agent.abort_sender();
    app.agent = Some(Arc::new(tokio::sync::Mutex::new(agent)));
    app.input = "synthetic prompt".into();
    app.submit();
    let started = std::time::Instant::now();
    loop {
        app.drain_events();
        if app
            .rows
            .iter()
            .any(|r| r.kind == RowKind::Tool && r.streaming)
        {
            break;
        }
        assert!(
            started.elapsed() < std::time::Duration::from_secs(3),
            "tool did not start"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let ctx = app.egui_ctx.clone();
    app.request_close();
    let started = std::time::Instant::now();
    loop {
        app.drain_events();
        let _ = ctx.run(Default::default(), |ctx| app.poll_exit(ctx));
        if app.exit_ready {
            break;
        }
        assert!(
            started.elapsed() < std::time::Duration::from_secs(3),
            "cooperative cancellation must finish within grace"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(
        !app.running,
        "real task completion, not the timeout, should allow close"
    );
    assert_eq!(app.last_stop, Some(StopReason::Aborted));
    assert!(app.agent.as_ref().unwrap().try_lock().is_ok());
    drop(app);
    let reopened = Session::open(&dir, &home).unwrap();
    assert!(reopened
        .messages()
        .iter()
        .any(|m| m.text() == "synthetic prompt"));
    assert!(reopened
        .messages()
        .iter()
        .any(|m| m.text().contains("cancelled by close")));
    assert!(
        reopened.turn() > 0,
        "cancelled tool turn must be snapshotted"
    );
    drop(reopened);
    std::fs::remove_dir_all(home).unwrap();
}

#[test]
fn retry_resets_only_live_assistant_and_never_duplicates_a_finished_tool() {
    let mut app = tests::fixture();
    app.rows
        .push(ChatRow::new(RowKind::Assistant, "prior answer"));
    let mut live = ChatRow::new(RowKind::Assistant, "failed partial");
    live.streaming = true;
    live.thinking = "failed thought".into();
    app.rows.push(live);
    app.on_agent_event(AgentEvent::ResponseReset {
        attempt: 2,
        max_attempts: 4,
        delay_ms: 5000,
        reason: "temporary connectivity".into(),
    });
    assert_eq!(app.rows[0].text, "prior answer");
    assert!(app.rows[1].text.is_empty());
    assert!(app.rows[1].thinking.is_empty());
    app.on_agent_event(AgentEvent::TextDelta("successful".into()));
    assert!(app.rows.iter().any(|r| r.text == "successful"));
    assert!(!app.rows.iter().any(|r| r.text.contains("failed partial")));
}

/// The footer must expose the traffic limit next to the proxy, and the popup
/// must be reachable and closable like the proxy picker.
#[test]
fn footer_shows_the_rate_limit_control_near_the_proxy() {
    let mut app = tests::fixture();
    let ctx = app.egui_ctx.clone();
    theme::apply_theme(&ctx);
    let mut rows = Vec::new();
    let out = ctx.run(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(950.0, 150.0),
            )),
            ..Default::default()
        },
        |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| app.status_bar(ui));
        },
    );
    for job in &out.shapes {
        texts(&job.shape, &mut rows);
    }
    let find = |needle: &str| {
        rows.iter()
            .find(|(label, _)| label.contains(needle))
            .map(|(_, rect)| *rect)
            .unwrap_or_else(|| panic!("footer is missing {needle:?}: {:?}", labels(&rows)))
    };
    let limit = find("Limit:");
    let proxy = find("Proxy:");
    assert!(
        limit.left() < proxy.left(),
        "the limit sits immediately left of the proxy at the bottom right"
    );
    // Only a separator may sit between them.
    let between = rows
        .iter()
        .filter(|(_, rect)| rect.left() > limit.right() && rect.right() < proxy.left())
        .count();
    assert!(between <= 1, "unexpected items between limit and proxy");

    // Clicking it opens the picker, and Escape closes it without applying.
    app.open_rate_limit_picker();
    assert!(matches!(
        app.request_picker,
        Some(request_choices::Picker::RateLimit { .. })
    ));
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
        |ctx| app.requests_window(ctx),
    );
    assert!(
        app.request_picker.is_none(),
        "Escape must dismiss the limit popup"
    );
    assert_eq!(
        app.limits.limit.bytes_per_second, 0,
        "dismissing must not change the stored limit"
    );
}

/// A typed limit must reach the shaper, persist, and clear again, while bad
/// input leaves the previous limit alone and reports why.
#[test]
fn rate_limit_choice_applies_persists_and_reports_bad_input() {
    let home = std::env::temp_dir().join(feather_core::provider::gen_id("gui-limit"));
    std::fs::create_dir_all(&home).unwrap();
    let mut app = tests::fixture();
    app.home = home.clone();
    app.limits = request_choices::Limits::new(&home);
    // The shared fixture is read-only preview mode; this test inspects a real
    // (temporary) home, so writes must actually happen.
    app.preview = false;
    let ctx = app.egui_ctx.clone();
    feather_core::provider::transport::set_rate_limit(0);

    app.open_rate_limit_picker();
    let picked = Some(request_choices::Choice::RateLimit(512 * 1024));
    app.apply_request_choice(picked, &ctx);
    assert_eq!(app.limits.limit.bytes_per_second, 512 * 1024);
    assert_eq!(
        feather_core::provider::transport::rate_limit(),
        512 * 1024,
        "the shaper must enforce the stored value"
    );
    assert_eq!(app.limits.label(), "Limit: 512 KB/s");
    assert_eq!(
        feather::netlimit::load(&home).bytes_per_second,
        512 * 1024,
        "the limit must survive a reload"
    );

    // Bad input: reported, and the active limit is untouched.
    app.open_rate_limit_picker();
    app.apply_request_choice(
        Some(request_choices::Choice::Error("Unknown unit 'TB'".into())),
        &ctx,
    );
    assert_eq!(app.limits.limit.bytes_per_second, 512 * 1024);
    assert!(app
        .limits
        .error
        .as_deref()
        .is_some_and(|e| e.contains("Unknown unit")));

    // Back to unlimited.
    app.open_rate_limit_picker();
    app.apply_request_choice(Some(request_choices::Choice::RateLimit(0)), &ctx);
    assert_eq!(app.limits.limit.bytes_per_second, 0);
    assert_eq!(app.limits.label(), "Limit: Off");
    assert_eq!(feather_core::provider::transport::rate_limit(), 0);
    std::fs::remove_dir_all(&home).unwrap();
}

fn labels(rows: &[(String, egui::Rect)]) -> Vec<String> {
    rows.iter().map(|(label, _)| label.clone()).collect()
}

#[test]
fn render_label_opens_a_modal_and_escape_does_not_submit_the_draft() {
    let mut app = tests::fixture();
    app.input = "keep draft".into();
    app.show_models = true;
    app.show_login = true;
    app.open_renderer_picker();
    assert!(app.renderer_picker.is_some());
    assert!(!app.show_models && !app.show_login);
    let ctx = app.egui_ctx.clone();
    app.handle_action("tui.input.submit", &ctx);
    assert_eq!(app.input, "keep draft");
    assert!(!app.running);
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
    assert!(app.renderer_picker.is_none());
    assert_eq!(app.input, "keep draft");
    assert!(app.exit.is_none());
}
