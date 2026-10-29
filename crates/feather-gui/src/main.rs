// GUI subsystem: no console window pops up when launched from Explorer.
// Debug builds keep the console for println debugging.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! Feather GUI — the desktop frontend (egui/eframe).
//!
//! Replaces the TUI as the primary interface: a calm, feather-light chat
//! window wired to the same agent core the CLI uses (`feather::wiring`).
//! Ports pi's keybinding system (`keybindings.rs`), model selection, and
//! OAuth login (`oauth.rs`).

mod activity;
mod app_runtime;
mod cards;
#[cfg(test)]
mod chrome_tests;
mod commands;
mod context_menu;
mod dialog;
mod font_store;
mod frame_pacing;
mod gpu_fallback;
#[cfg(test)]
mod gpu_layout_tests;
mod graphics;
mod history;
mod keybindings;
mod lifecycle;
#[cfg(test)]
mod lifecycle_tests;
mod metrics;
mod model_manager;
#[cfg(test)]
mod picker_tests;
mod preview_timing;
#[cfg(test)]
mod queue_tests;
mod renderer_choice;
mod request_choices;
#[cfg(test)]
mod request_tests;
#[cfg(test)]
mod requests_010_tests;
mod rewind_choices;
#[cfg(test)]
mod scroll_tests;
mod sessions;
mod skill_choices;
mod software_painter;
mod software_window;
#[cfg(test)]
mod stability_tests;
mod suggestion_cache;
#[cfg(test)]
mod tests;
mod theme;
mod transcript;
mod window_chrome;
mod workspace;
use theme::{
    apply_theme, install_fonts, soft_shadow, INK, MUTED, PANEL, PANEL_2, RED, SIDEBAR, SOFT_ACCENT,
    TEAL, TEXT,
};
use dialog::{
    button, check_row, choice_row, effort_bar, error_line, field, footnote, hint, list_row, search,
    segmented, sheet, tag, ButtonKind, Scrim,
};

use feather::oauth;

use crossbeam_channel::Sender;
use eframe::egui;
use feather::config::{self, Settings};
use feather::models;
use feather::wiring;
use feather_core::agent::{Agent, AgentEvent, RunResult};
use feather_core::message::{Role, StopReason};
use std::sync::Arc;

const LOGO_PNG: &[u8] = include_bytes!("../assets/feather-logo.png");

// ---------------------------------------------------------------------------
// transcript model
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum RowKind {
    User,
    Assistant,
    Tool,
    Error,
    Notice,
}

#[derive(Debug, Clone)]
struct ChatRow {
    ui_key: u64,
    kind: RowKind,
    text: String,
    thinking: String,
    tool_output: Option<String>,
    tool_error: bool,
    streaming: bool,
    tool_id: Option<String>,
    tool_args: String,
    started_at: Option<f64>,
    elapsed: Option<f64>,
    expanded: bool,
}

impl ChatRow {
    fn new(kind: RowKind, text: impl Into<String>) -> Self {
        static ROW_KEY: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        ChatRow {
            ui_key: ROW_KEY.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            kind,
            text: text.into(),
            thinking: String::new(),
            tool_output: None,
            tool_error: false,
            streaming: false,
            tool_id: None,
            tool_args: String::new(),
            started_at: None,
            elapsed: None,
            expanded: false,
        }
    }
}

// ---------------------------------------------------------------------------
// UI events (agent thread -> egui thread)
// ---------------------------------------------------------------------------

enum UiEvent {
    Agent(AgentEvent),
    RunDone(Result<RunResult, String>),
    Auth {
        provider: &'static str,
        action: &'static str,
        result: Result<(), String>,
    },
    /// Startup model discovery finished; the catalog on screen is stale.
    ModelsRefreshed,
}

// ---------------------------------------------------------------------------
// app
// ---------------------------------------------------------------------------

struct FeatherApp {
    rt: app_runtime::UiRuntime,
    agent: Option<Arc<tokio::sync::Mutex<Agent>>>,
    abort_tx: tokio::sync::watch::Sender<bool>,
    input_queue: feather_core::agent::InputQueue,
    ev_tx: Sender<UiEvent>,
    ev_rx: crossbeam_channel::Receiver<UiEvent>,
    egui_ctx: egui::Context,

    settings: Settings,
    root: std::path::PathBuf,
    home: std::path::PathBuf,
    session_dir: std::path::PathBuf,

    rows: Vec<ChatRow>,
    history: history::History,
    input: String,
    running: bool,
    last_stop: Option<StopReason>,
    queued_hint: usize,
    /// Startup chooser; `None` once a choice is made (or when on the welcome).
    picker: Option<sessions::SessionChooser>,

    kb: keybindings::Keybindings,
    show_models: bool,
    show_settings: bool,
    show_login: bool,
    show_thinking_picker: bool,
    thinking_slider: usize,
    thinking_applied: usize,
    expand_tools: bool,
    show_thinking: bool,

    auth: oauth::AuthStore,
    api_key_drafts: std::collections::BTreeMap<String, String>,
    auth_pending: std::collections::BTreeSet<&'static str>,
    auth_changed: bool,
    catalog: Option<feather::models::ModelsFile>,
    metrics: metrics::StreamingMetrics,
    last_usage: feather_core::message::Usage,
    preview: bool,
    model_filter: String,
    auth_selected: usize,
    model_cursor: usize,
    model_manager: model_manager::ModelManager,
    command_cursor: usize,
    capture_frames: u32,
    preview_timing: preview_timing::PreviewTiming,
    frame_pacer: frame_pacing::Pacer,
    rewind_turns: rewind_choices::Turns,
    skill_choices: skill_choices::Skills,
    suggestions: suggestion_cache::Suggestions,
    renderer: renderer_choice::Backend,
    renderer_picker: Option<renderer_choice::Picker>,
    request_picker: Option<request_choices::Picker>,
    proxies: request_choices::Proxies,
    limits: request_choices::Limits,
    exit: Option<lifecycle::Exit>,
    exit_ready: bool,
}

impl FeatherApp {
    fn new(cc: &eframe::CreationContext<'_>, root: std::path::PathBuf) -> anyhow::Result<Self> {
        window_chrome::round_native_window(cc);
        graphics::report_adapter(cc);
        let mut app = Self::new_with_context(cc.egui_ctx.clone(), root)?;
        app.frame_pacer.set_window(cc);
        Ok(app)
    }

    fn new_with_context(ctx: egui::Context, root: std::path::PathBuf) -> anyhow::Result<Self> {
        install_fonts(&ctx);
        apply_theme(&ctx);

        let preview = std::env::var("FEATHER_UI_PREVIEW").ok().as_deref() == Some("1");
        let mut settings = if preview {
            Settings::default()
        } else {
            Settings::load()
        };
        if preview {
            settings.provider = "faux".into();
        }
        let home = if preview {
            std::env::temp_dir().join(format!("feather-preview-{}", std::process::id()))
        } else {
            config::feather_home()
        };
        std::fs::create_dir_all(&home)?;
        // A renderer handoff must supersede normal auto-resume/chooser startup;
        // never open a different (newest) session before restoring the chosen one.
        let handoff = lifecycle::startup_handoff()?;

        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .max_blocking_threads(8)
            .enable_all()
            .build()?;
        let (ev_tx, ev_rx) = crossbeam_channel::unbounded();

        // Listing is read-only, so no `session.redb` appears before a choice.
        let history = if preview || handoff.is_some() {
            Vec::new()
        } else {
            sessions::list(&home, &root)
        };
        let mut agent = None;
        let mut picker = None;
        let mut abort_tx = tokio::sync::watch::channel(false).0;
        let mut input_queue = feather_core::agent::InputQueue::default();
        let mut session_dir = std::path::PathBuf::new();
        let mut rows = Vec::new();
        let mut transcript_history = history::History::default();
        let mut rewind_turns = rewind_choices::Turns::default();
        let mut last_usage = feather_core::message::Usage::default();
        let plan = if handoff.is_some() {
            sessions::StartupPlan::Welcome
        } else {
            sessions::startup_plan(history.is_empty(), settings.auto_resume_latest, preview)
        };
        match plan {
            sessions::StartupPlan::Preview => {}
            sessions::StartupPlan::ResumeLatest => {
                let resume = !preview;
                let (built, _tasks) = wiring::make_agent(&root, &settings, resume)?;
                if resume {
                    let (loaded_history, loaded_rows) =
                        history::History::from_session(built.session())
                            .map_err(anyhow::Error::msg)?;
                    transcript_history = loaded_history;
                    rows = loaded_rows;
                    last_usage = built.session().total_usage();
                }
                session_dir = built.session().dir().to_path_buf();
                abort_tx = built.abort_sender();
                input_queue = built.input_queue();
                rewind_turns.attach(built.session().snapshots().cloned());
                let built = Arc::new(tokio::sync::Mutex::new(built));
                agent = Some(built);
            }
            sessions::StartupPlan::Choose => {
                picker = Some(sessions::SessionChooser::new(history));
            }
            sessions::StartupPlan::Welcome => {}
        }
        // An install can be complete while the host is not: Bash and Git are
        // deliberately not bundled. Say so here, once, with the fix — the
        // alternative is a mysterious failure on the first Bash call.
        if !preview {
            if let Some(warning) = feather::prereq::warning() {
                rows.push(ChatRow::new(RowKind::Error, warning));
            }
        }

        let kb = if preview {
            keybindings::Keybindings::load_from(&home.join("config.redb"))
        } else {
            keybindings::Keybindings::load()
        };
        let renderer = renderer_choice::selected(&home, preview);
        let mut app = FeatherApp {
            rt: rt.into(),
            agent,
            abort_tx,
            input_queue,
            ev_tx,
            ev_rx,
            egui_ctx: ctx,
            settings,
            root,
            home,
            session_dir,
            rows,
            history: transcript_history,
            input: String::new(),
            running: false,
            last_stop: None,
            queued_hint: 0,
            picker,
            kb,
            show_models: false,
            show_settings: false,
            show_login: false,
            show_thinking_picker: false,
            thinking_slider: 3,
            thinking_applied: 3,
            expand_tools: false,
            show_thinking: true,
            auth: if preview {
                Default::default()
            } else {
                oauth::AuthStore::load()
            },
            api_key_drafts: Default::default(),
            auth_pending: Default::default(),
            auth_changed: false,
            catalog: if preview { None } else { models::load() },
            metrics: Default::default(),
            last_usage,
            preview,
            model_filter: String::new(),
            auth_selected: 0,
            model_cursor: 0,
            model_manager: {
                let mut manager = model_manager::ModelManager::default();
                manager.enabled = true;
                manager
            },
            renderer,
            renderer_picker: None,
            request_picker: None,
            proxies: if preview {
                Default::default()
            } else {
                request_choices::Proxies::new(&config::feather_home())
            },
            limits: if preview {
                Default::default()
            } else {
                request_choices::Limits::new(&config::feather_home())
            },
            exit: None,
            exit_ready: false,
            command_cursor: 0,
            capture_frames: 0,
            preview_timing: Default::default(),
            frame_pacer: Default::default(),
            rewind_turns,
            skill_choices: Default::default(),
            suggestions: Default::default(),
        };
        if preview && std::env::var("FEATHER_UI_PREVIEW_EMPTY").ok().as_deref() != Some("1") {
            app.preview_rows();
        }
        if preview {
            if std::env::var("FEATHER_UI_PREVIEW_PANEL").ok().as_deref() == Some("skills") {
                // Metadata-only fixture in the isolated preview home. Never copy
                // private skill instructions or configure the real user home.
                for (name, description) in [
                    ("docx", "Create and edit Word documents"),
                    ("pptx", "Create and edit presentations"),
                    ("xlsx", "Create and edit spreadsheets"),
                ] {
                    let dir = app.home.join("skills").join(name);
                    std::fs::create_dir_all(&dir)?;
                    std::fs::write(dir.join("SKILL.md"),format!("---\nname: {name}\ndescription: {description}\n---\nSynthetic preview only."))?;
                }
            }
            match std::env::var("FEATHER_UI_PREVIEW_PANEL")
                .unwrap_or_default()
                .as_str()
            {
                "add" => {
                    app.show_login = true;
                    app.model_manager.enabled = true;
                }
                "thinking" => app.show_thinking_picker = true,
                "renderer" => app.open_renderer_picker(),
                "proxy" => app.open_request_picker(false),
                "skills" => app.input = "/skill ".into(),
                "skills-modal" => app.open_request_picker(true),
                "commands" => app.input = "/".into(),
                "rewind" => {
                    app.input = "/rewind ".into();
                    app.rewind_turns.loaded = true;
                    app.rewind_turns.rows = vec![
                        (
                            4,
                            "Fix OAuth persistence and migrate settings to redb".into(),
                        ),
                        (3, "Align the footer and add proxy selection".into()),
                        (2, "Review the GPU renderer and memory use".into()),
                        (1, "Inspect the workspace".into()),
                    ];
                }
                _ => {}
            }
        }
        if let Some(handoff) = handoff {
            if !preview && !handoff.session.as_os_str().is_empty() {
                if handoff.session.parent() != Some(app.home.join("sessions").as_path()) {
                    anyhow::bail!("renderer restart session is outside Feather home");
                }
                app.choose_pick(sessions::Pick::Resume(handoff.session));
            }
            app.input = handoff.draft;
            app.show_thinking = handoff.show_thinking;
            app.expand_tools = handoff.expand_tools;
            for row in &mut app.rows {
                if row.kind == RowKind::Tool {
                    row.expanded = app.expand_tools;
                }
            }
        }
        if preview
            && std::env::var("FEATHER_UI_PREVIEW_BLOCKING_SHUTDOWN")
                .ok()
                .as_deref()
                == Some("1")
        {
            // Reproduce Tokio's formerly unbounded blocking-job destructor wait.
            app.rt
                .spawn_blocking(|| std::thread::sleep(std::time::Duration::from_secs(60)));
        }
        if !preview {
            app.proxies.scan(app.home.clone(), app.egui_ctx.clone());
            feather::model_refresh::spawn_startup_refresh(app.home.clone(), {
                let tx = app.ev_tx.clone();
                let ctx = app.egui_ctx.clone();
                move || {
                    let _ = tx.send(UiEvent::ModelsRefreshed);
                    ctx.request_repaint();
                }
            });
        }
        Ok(app)
    }

    fn manage_model(&mut self, action: model_manager::Action) {
        if self.preview {
            self.model_manager.error =
                Some("Preview mode is read-only; no configuration was saved.".into());
            return;
        }
        use model_manager::Action;
        let result = match action {
            Action::Save(pid, p, mut m) => {
                // Preserve existing image support, compatibility maps and other
                // advanced metadata when editing the visible form.
                if let Some(old) = self
                    .catalog
                    .as_ref()
                    .and_then(|c| c.providers.get(&pid))
                    .and_then(|p| p.models.iter().find(|v| v.id == m.id))
                {
                    let mut preserved = old.clone();
                    preserved.name = m.name;
                    preserved.reasoning_not_supported = m.reasoning_not_supported;
                    preserved.max_tokens = m.max_tokens;
                    // Context is a single global setting now: model entries keep no
                    // per-model budget, so editing a model cannot reintroduce one.
                    preserved.context_window = None;
                    m = preserved;
                }
                match models::upsert_model(&pid, p, m) {
                    Ok(()) => {
                        self.catalog = models::load();
                        self.auth_changed = true;
                        Ok(())
                    }
                    Err(e) => Err(e),
                }
            }
            Action::SaveContext(context, reserve, preserved) => {
                match models::validate_context_limits(context, reserve, preserved) {
                    Ok(()) => {
                        self.settings.context_window = context;
                        self.settings.compaction.reserve_tokens = reserve;
                        self.settings.compaction.keep_recent_tokens = preserved;
                        self.apply_settings_change();
                        Ok(())
                    }
                    Err(e) => Err(e),
                }
            }

            Action::Delete(pid, id) => match models::delete_model(&pid, &id) {
                Ok(()) => {
                    self.catalog = models::load();
                    if self.settings.model == format!("{pid}/{id}") {
                        let next = self.catalog.as_ref().and_then(|c| {
                            c.providers.iter().find_map(|(pid, p)| {
                                if self.provider_available(pid, p) {
                                    p.models.first().map(|m| format!("{pid}/{}", m.id))
                                } else {
                                    None
                                }
                            })
                        });
                        self.settings.model = next.unwrap_or_default();
                        self.apply_settings_change();
                    }
                    Ok(())
                }
                Err(e) => Err(e),
            },
        };
        self.model_manager.invalidate_catalog();
        self.model_manager.error = result.err().map(|e| e.to_string());
    }
    fn now(&self) -> f64 {
        self.egui_ctx.input(|i| i.time)
    }

    /// Offline visual-regression fixture. Never sends a model request.
    fn preview_rows(&mut self) {
        if std::env::var("FEATHER_UI_PREVIEW_STRESS").ok().as_deref() == Some("1") {
            // Synthetic long, diverse glyph history for native memory/frame tests.
            let chars: String = (0..7000)
                .filter_map(|i| char::from_u32(0x4E00 + i))
                .collect();
            let mut row = ChatRow::new(RowKind::Assistant, "Long-text rendering stress preview");
            row.thinking = chars.repeat(20);
            self.rows.push(row);
            return;
        }
        self.settings.model = "openai-codex/gpt-5.4".into();
        self.rows.push(ChatRow::new(
            RowKind::User,
            "请帮我检查登录功能，并整理一下界面。",
        ));
        let mut assistant = ChatRow::new(RowKind::Assistant, "让我们轻一点，把复杂留给 Feather。\n\n登录凭证已接入每次请求，工具循环和子任务也会自动续期。现在检查本地配置与测试结果。");
        assistant.thinking =
            "先核对凭证来源，再验证刷新与模型路由；不会读取或显示真实密钥。".into();
        self.rows.push(assistant);
        for (name, args, output, fail, expanded, seconds) in [
            ("read", "crates/feather/src/auth.rs", "auth│StoredCredentialResolver\nnext│Request-time token refresh\nsafe│Credentials never appear in the transcript", false, true, 0.2),
            ("bash", "cargo test -p feather auth", "9 passed · 0 failed\nCredential rotation and concurrent refresh verified.", false, false, 1.8),
            ("read", "notes/draft.md", "File not found. The workspace and current session were not changed.", true, false, 0.1),
        ] {
            let mut row = ChatRow::new(RowKind::Tool, name);
            row.tool_id = Some(format!("preview-{}", self.rows.len()));
            row.tool_args = args.into(); row.tool_output = Some(output.into());
            row.tool_error = fail; row.elapsed = Some(seconds); row.expanded = expanded;
            self.rows.push(row);
        }
        self.rows.push(ChatRow::new(RowKind::Assistant, "检查完成。登录后无需重新打开会话，新的凭证会用于下一次请求。\n\n你可以继续输入任务，或按 Ctrl+L 选择模型。"));
        self.metrics.begin_run(0.0);
        self.metrics.begin_turn(0.0);
        self.metrics.record_delta("preview", 1.0);
        self.metrics.finish_turn(128, 5.0);
        self.metrics.finish_run(5.0);
        self.last_usage = feather_core::message::Usage {
            input: 2416,
            output: 128,
            cache_read: 512,
            cache_write: 0,
        };
    }

    // -- agent lifecycle ---------------------------------------------------

    /// Rebuild the agent (model/thinking changed or new session). Keeps the
    /// session when `keep_session_dir` is set — the transcript persists, so
    /// nothing is lost.

    fn rebuild_agent(&mut self, new_session: bool) {
        self.running = false;
        // No session chosen yet: the lazy agent on submit picks up the current
        // settings, so there is nothing to rebuild here.
        if self.agent.is_none() && !new_session {
            return;
        }
        // Drop the previous agent BEFORE touching the session file: the old
        // Agent owns the redb database, and redb takes an exclusive file lock.
        // A still-running run() task may hold the Arc for a few more
        // milliseconds, so retry the open briefly.
        self.rewind_turns = Default::default();
        let old = self.agent.take();
        drop(old);
        let result = if new_session {
            wiring::make_agent(&self.root.clone(), &self.settings, false)
                .map_err(anyhow::Error::from)
        } else {
            self.reopen_session_with_retry()
        };
        match result {
            Ok((agent, tasks)) => {
                self.install_agent(agent, tasks);
                if new_session {
                    self.rows.clear();
                    // install_agent keeps the new durable audit pager.
                    self.last_stop = None;
                    self.metrics = Default::default();
                    self.last_usage = Default::default();
                    self.queued_hint = 0;
                }
            }
            Err(e) => {
                self.rows.push(ChatRow::new(
                    RowKind::Error,
                    format!("agent rebuild failed: {e}"),
                ));
            }
        }
    }

    /// Reopen the current session, retrying while the old agent's redb lock
    /// drains (agent drop is asynchronous when a run task still holds the Arc).
    fn reopen_session_with_retry(
        &mut self,
    ) -> anyhow::Result<(Agent, Arc<feather::tasks::TaskStore>)> {
        let mut last_err = None;
        for attempt in 0..20 {
            match feather_core::session::Session::open(&self.session_dir, &config::feather_home()) {
                Ok(session) => {
                    return wiring::make_agent_for_session(
                        &self.root.clone(),
                        &self.settings,
                        session,
                    )
                    .map_err(anyhow::Error::from);
                }
                Err(e) => {
                    let msg = e.to_string();
                    if !msg.contains("already open") && attempt > 2 {
                        return Err(anyhow::anyhow!("reopen session: {e}"));
                    }
                    last_err = Some(e);
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
            }
        }
        Err(anyhow::anyhow!(
            "reopen session: {:?}",
            last_err.map(|e| e.to_string()).unwrap_or_default()
        ))
    }

    // -- session chooser ---------------------------------------------------

    /// `Ctrl+Shift+R` / footer click: toggle the workspace session chooser.
    fn open_session_picker(&mut self) {
        if self.running {
            return;
        }
        if self.picker.is_some() {
            self.picker = None;
            return;
        }
        self.show_picker(sessions::list(&self.home, &self.root));
    }

    /// Inject chooser rows without touching disk. This is also the test seam;
    /// the app never creates a session until the user resolves the choice.
    fn show_picker(&mut self, sessions: Vec<feather_core::session_catalog::SessionInfo>) {
        if sessions.is_empty() {
            self.picker = None;
            return;
        }
        self.picker = Some(sessions::SessionChooser::new(sessions));
        self.show_models = false;
        self.show_settings = false;
        self.show_login = false;
        self.show_thinking_picker = false;
        self.renderer_picker = None;
        self.request_picker = None;
        self.egui_ctx
            .memory_mut(|m| m.surrender_focus(egui::Id::new("feather-composer")));
    }

    /// Resolve one chooser selection. Only reachable while idle.
    fn choose_pick(&mut self, pick: sessions::Pick) {
        if self.running {
            return;
        }
        match pick {
            // A fresh session is *not* durable yet: submit materializes it.
            sessions::Pick::NewSession => {
                self.start_new_session();
            }
            sessions::Pick::Resume(dir) => self.switch_to_session(dir),
            sessions::Pick::Delete(dir) => {
                // Active session must be explicitly left before deleting it.
                let result = if dir == self.session_dir {
                    Err(
                        "This session is open. Start a new session, then delete it from history."
                            .to_string(),
                    )
                } else {
                    feather_core::session_catalog::delete_session(&self.home, &dir)
                        .map_err(|e| e.to_string())
                };
                match result {
                    Ok(()) => {
                        self.picker = Some(sessions::SessionChooser::new(sessions::list(
                            &self.home, &self.root,
                        )))
                    }
                    Err(e) => {
                        if let Some(p) = &mut self.picker {
                            p.error = Some(e);
                        }
                    }
                }
            }
        }
    }

    /// Reopen an existing session, releasing the old redb lock first.
    fn switch_to_session(&mut self, dir: std::path::PathBuf) {
        if self.running {
            return;
        }
        self.picker = None;
        self.rewind_turns = Default::default();
        let old = self.agent.take();
        drop(old);
        self.session_dir = dir;
        match self.reopen_session_with_retry() {
            Ok((agent, tasks)) => {
                let (history, rows) = match history::History::from_session(agent.session()) {
                    Ok(loaded) => loaded,
                    Err(error) => {
                        self.rows.push(ChatRow::new(
                            RowKind::Error,
                            format!("load transcript: {error}"),
                        ));
                        return;
                    }
                };
                let usage = agent.session().total_usage();
                let stop = agent
                    .session()
                    .messages()
                    .iter()
                    .rev()
                    .find_map(|message| message.stop_reason);
                self.install_agent(agent, tasks);
                self.rows = rows;
                self.history = history;
                self.last_usage = usage;
                self.last_stop = stop;
                self.metrics = Default::default();
                self.queued_hint = 0;
            }
            Err(e) => self
                .rows
                .push(ChatRow::new(RowKind::Error, format!("open session: {e}"))),
        }
    }

    /// `Ctrl+Shift+N`: a new session. With no agent we are already on the
    /// in-memory welcome, so nothing durable is created here.
    fn start_new_session(&mut self) {
        if self.running {
            return;
        }
        self.picker = None;
        self.agent = None;
        self.rewind_turns = Default::default();
        self.history = history::History::default();
        self.session_dir.clear();
        self.rows.clear();
        self.last_stop = None;
        self.last_usage = Default::default();
        self.metrics = Default::default();
        self.queued_hint = 0;
        self.input_queue = Default::default();
    }

    /// Lazily materialize the durable session for the first submit.
    fn ensure_agent(&mut self) -> bool {
        if self.agent.is_some() {
            return true;
        }
        match wiring::make_agent(&self.root.clone(), &self.settings, false) {
            Ok((agent, tasks)) => {
                self.install_agent(agent, tasks);
                true
            }
            Err(e) => {
                self.rows.push(ChatRow::new(
                    RowKind::Error,
                    format!("session start failed: {e}"),
                ));
                false
            }
        }
    }

    /// Swap in a freshly built agent, rebinding its queues and event pump.
    fn install_agent(&mut self, agent: Agent, _tasks: Arc<feather::tasks::TaskStore>) {
        self.rewind_turns = Default::default();
        let agent = Arc::new(tokio::sync::Mutex::new(agent));
        {
            let guard = agent.blocking_lock();
            self.input_queue = guard.input_queue();
            self.abort_tx = guard.abort_sender();
            self.session_dir = guard.session().dir().to_path_buf();
            self.rewind_turns
                .attach(guard.session().snapshots().cloned());
            if let Ok((history, _)) = history::History::from_session(guard.session()) {
                self.history = history;
            }
        }
        self.agent = Some(agent);
        self.auth_changed = false;
    }

    fn reconfigure_auth(&mut self) -> Result<(), String> {
        let agent = self.agent.as_ref().ok_or("no agent")?;
        let mut agent = agent.try_lock().map_err(|_| "agent is busy; try again")?;
        let (config, provider, tools, _) =
            wiring::runtime_components(&self.root, &self.settings, agent.session().dir())
                .map_err(|e| format!("auth runtime: {e}"))?;
        agent.reconfigure(config, provider, tools);
        self.auth_changed = false;
        Ok(())
    }

    // -- send / queue ------------------------------------------------------

    fn execute_command(&mut self, input: &str) {
        let Some((name, args)) = commands::parse(input) else {
            return;
        };
        let fail = |app: &mut Self, text: String| app.rows.push(ChatRow::new(RowKind::Error, text));
        if self.running && !matches!(name, "stop" | "help" | "thoughts" | "tools") {
            fail(
                self,
                "Stop the response before running this command.".into(),
            );
            return;
        }
        match name {
            "skill" if args.is_empty() => self.open_request_picker(true),
            "proxy" => self.open_request_picker(false),
            "new" => self.start_new_session(),
            "resume" => self.open_session_picker(),
            "model" if args.is_empty() => self.toggle_models(),
            "model" => {
                if let Some((pid, p, m)) = self.catalog.as_ref().and_then(|c| models::find(c, args))
                {
                    let (pid, id) = (pid.to_string(), m.id.clone());
                    let _ = p;
                    self.select_model(&pid, &id);
                } else {
                    fail(self, format!("Unknown model: {args}"));
                }
            }
            "think" | "thinking" if args.is_empty() => self.open_thinking_picker(),
            "think" | "thinking" => {
                let level = if args == "med" { "medium" } else { args };
                if self.effort_levels().iter().any(|v| v == level) {
                    self.set_thinking(level);
                } else {
                    fail(self, format!("Unsupported effort: {args}"));
                }
            }
            "add" => {
                self.show_login = true;
                self.model_manager.enabled = true;
            }
            "settings" | "hotkeys" => self.show_settings = true,
            "help" => self.rows.push(ChatRow::new(
                RowKind::Notice,
                commands::COMMANDS
                    .iter()
                    .map(|c| format!("/{} — {}", c.name, c.description))
                    .collect::<Vec<_>>()
                    .join("\n"),
            )),
            "thoughts" => self.show_thinking = !self.show_thinking,
            "tools" => {
                self.expand_tools = !self.expand_tools;
                for row in &mut self.rows {
                    if row.kind == RowKind::Tool {
                        row.expanded = self.expand_tools;
                    }
                }
            }
            "stop" => self.abort(),
            "quit" => self.request_close(),
            "snapshot" => {
                let turn = self
                    .agent
                    .as_ref()
                    .and_then(|a| a.try_lock().ok().map(|a| a.session().turn()))
                    .unwrap_or(0);
                self.rows.push(ChatRow::new(
                    RowKind::Notice,
                    format!("Current committed turn: {turn}"),
                ));
            }
            "rewind" if args.is_empty() => {
                self.input = "/rewind ".into();
                self.rewind_turns.request(&self.egui_ctx);
            }
            "rewind" => {
                let result = (|| {
                    let turn = args
                        .parse::<u64>()
                        .map_err(|_| "Usage: /rewind <turn>".to_string())?;
                    let agent = self.agent.as_ref().ok_or("No active session")?;
                    let mut a = agent.try_lock().map_err(|_| "Agent is busy")?;
                    a.session_mut().rewind_to(turn).map_err(|e| e.to_string())?;
                    history::History::from_session(a.session())
                })();
                match result {
                    Ok((history, rows)) => {
                        self.rewind_turns.invalidate();
                        self.history = history;
                        self.rows = rows;
                        self.rows.push(ChatRow::new(RowKind::Notice,format!("Rewound LLM context to turn {args}. Original conversation is retained in history.")));
                    }
                    Err(e) => fail(self, e),
                }
            }
            _ => fail(self, format!("Unknown command /{name}. Use /help.")),
        }
    }
    fn submit(&mut self) {
        if self.exit.is_some() {
            return;
        }
        let skill_invocation = self.input.starts_with("/skill:")
            || commands::parse(&self.input)
                .is_some_and(|(name, args)| name == "skill" && !args.is_empty());
        if self.input.starts_with('/') && !skill_invocation {
            let command = std::mem::take(&mut self.input);
            self.execute_command(&command);
            return;
        }
        let text = if skill_invocation {
            match config::resources_for(&self.root, &self.home)
                .expand_user_prompt(self.input.trim())
            {
                Ok(text) => text,
                Err(error) => {
                    self.rows.push(ChatRow::new(RowKind::Error, error));
                    return;
                }
            }
        } else {
            self.input.trim().to_string()
        };
        if text.is_empty() {
            return;
        }
        self.input.clear();
        if self.running {
            // The thread-safe inbox remains writable while run() owns Agent.
            self.input_queue
                .push(feather_core::message::Message::user_text(text), false);
            self.queued_hint += 1;
            return;
        }
        if self.agent.is_some() && self.auth_changed {
            // Only an offline->live transition needs runtime reconfiguration.
            // Credential renewal itself never reopens the redb session.
            if let Err(e) = self.reconfigure_auth() {
                self.input = text;
                self.rows.push(ChatRow::new(RowKind::Error, e));
                return;
            }
        }
        // The durable session is created here — the first submit after the
        // welcome — never at startup, so no empty `session.redb` can appear.
        if self.agent.is_none() && !self.ensure_agent() {
            self.input = text;
            return;
        }
        self.rows.push(ChatRow::new(RowKind::User, text.clone()));
        let Some(agent) = self.agent.clone() else {
            self.rows
                .push(ChatRow::new(RowKind::Error, String::from("no agent")));
            return;
        };
        let tx = self.ev_tx.clone();
        // Reset before acceptance, not inside the first async poll: an immediate
        // cancel must not be overwritten by a late prompt reset.
        let _ = self.abort_tx.send(false);
        self.running = true;
        let now = self.now();
        self.metrics.begin_run(now);
        let ctx = self.egui_ctx.clone();
        self.rt.spawn(drive_prompt(agent, text, tx, ctx));
    }

    fn request_close(&mut self) {
        if self.exit.is_none() {
            self.abort();
            self.exit = Some(lifecycle::Exit::close());
            self.egui_ctx.request_repaint();
        }
    }

    fn open_request_picker(&mut self, skills: bool) {
        if self.exit.is_some() {
            return;
        }
        self.show_models = false;
        self.show_settings = false;
        self.show_login = false;
        self.show_thinking_picker = false;
        self.renderer_picker = None;
        self.picker = None;
        self.request_picker = Some(if skills {
            request_choices::Picker::Skills {
                resources: config::resources_for(&self.root, &self.home),
                filter: String::new(),
                cursor: 0,
            }
        } else {
            request_choices::Picker::Proxy {
                address: String::new(),
                cursor: self
                    .proxies
                    .catalog
                    .entries
                    .iter()
                    .position(|e| e.endpoint.route == self.proxies.catalog.selected)
                    .map_or(0, |n| n + 1),
                changed: false,
            }
        });
        self.egui_ctx
            .memory_mut(|m| m.surrender_focus(egui::Id::new("feather-composer")));
    }
    /// Open the traffic-limit popup, pre-filled with the current value.
    fn open_rate_limit_picker(&mut self) {
        if self.exit.is_some() {
            return;
        }
        self.show_models = false;
        self.show_settings = false;
        self.show_login = false;
        self.show_thinking_picker = false;
        self.renderer_picker = None;
        self.picker = None;
        // Highlight the preset matching the active limit, if any.
        let current = self.limits.limit.bytes_per_second;
        let selected = feather::netlimit::SUGGESTED
            .iter()
            .position(|(_, value)| {
                feather::netlimit::parse(value).is_ok_and(|bytes| bytes == current)
            })
            .unwrap_or(usize::MAX);
        self.request_picker = Some(request_choices::Picker::RateLimit {
            input: self.limits.limit.value_text(),
            selected,
        });
        self.egui_ctx
            .memory_mut(|m| m.surrender_focus(egui::Id::new("feather-composer")));
    }
    fn requests_window(&mut self, ctx: &egui::Context) {
        let Some(picker) = &mut self.request_picker else {
            return;
        };
        let mut open = true;
        let choice = request_choices::window(ctx, picker, &self.proxies, &self.limits, &mut open);
        self.apply_request_choice(choice, ctx);
        if !open {
            self.request_picker = None;
        }
    }
    /// Act on one picker result. Split out of `requests_window` so the choices
    /// can be tested without driving a frame.
    fn apply_request_choice(
        &mut self,
        choice: Option<request_choices::Choice>,
        ctx: &egui::Context,
    ) {
        match choice {
            Some(request_choices::Choice::Skill(name)) => {
                self.input = format!("/skill:{name} ");
                self.request_picker = None;
                ctx.memory_mut(|m| m.request_focus(egui::Id::new("feather-composer")));
            }
            Some(request_choices::Choice::Proxy(route)) => {
                if self.preview {
                    self.proxies.error =
                        Some("Preview mode is read-only; proxy was not saved.".into());
                    return;
                }
                let result = (|| {
                    if let feather_core::provider::transport::Route::Proxy { url } = &route {
                        if !self
                            .proxies
                            .catalog
                            .entries
                            .iter()
                            .any(|e| e.endpoint.route == route)
                        {
                            feather::proxy::add_manual(&self.home, url)?;
                        }
                    }
                    feather::proxy::save_selection(&self.home, route)
                })();
                match result {
                    Ok(catalog) => {
                        self.proxies.catalog = catalog;
                        self.request_picker = None;
                    }
                    Err(error) => self.proxies.error = Some(error),
                }
            }
            Some(request_choices::Choice::Remove(route)) => {
                if self.preview {
                    self.proxies.error =
                        Some("Preview mode is read-only; proxy was not removed.".into());
                    return;
                }
                match feather::proxy::remove(&self.home, route) {
                    Ok(catalog) => self.proxies.catalog = catalog,
                    Err(error) => self.proxies.error = Some(error),
                }
            }
            Some(request_choices::Choice::RateLimit(bytes)) => {
                if self.preview {
                    self.limits.error =
                        Some("Preview mode is read-only; the limit was not saved.".into());
                    return;
                }
                self.limits.save(&self.home, bytes);
            }
            Some(request_choices::Choice::Error(error)) => {
                // The same popup slot reports both proxy and limit problems, so
                // the message belongs with whichever picker is open.
                if matches!(
                    self.request_picker,
                    Some(request_choices::Picker::RateLimit { .. })
                ) {
                    self.limits.error = Some(error);
                } else {
                    self.proxies.error = Some(error);
                }
            }
            Some(request_choices::Choice::Scan) if !self.preview => {
                self.proxies.scan(self.home.clone(), ctx.clone())
            }
            Some(request_choices::Choice::Scan) => {
                self.proxies.error = Some("Preview mode is read-only; no scan was started.".into())
            }
            None => {}
        }
    }

    fn open_renderer_picker(&mut self) {
        if self.exit.is_some() {
            return;
        }
        if self.renderer_picker.is_some() {
            self.renderer_picker = None;
            return;
        }
        self.renderer_picker = Some(renderer_choice::Picker::new(self.renderer));
        self.show_models = false;
        self.show_settings = false;
        self.show_login = false;
        self.request_picker = None;
        self.show_thinking_picker = false;
        self.picker = None;
        self.egui_ctx
            .memory_mut(|m| m.surrender_focus(egui::Id::new("feather-composer")));
    }

    fn renderer_window(&mut self, ctx: &egui::Context) {
        let Some(picker) = &mut self.renderer_picker else {
            return;
        };
        // Computed only while the picker is open: this runs per frame, and
        // `try_lock` otherwise contends with a streaming response for nothing.
        let enabled = self.exit.is_none()
            && !self.running
            && self.auth_pending.is_empty()
            && self.agent.as_ref().map_or(true, |a| a.try_lock().is_ok());
        let mut open = true;
        let picked = renderer_choice::picker_window(ctx, picker, self.renderer, enabled, &mut open);
        if let Some(backend) = picked {
            self.renderer_picker = None;
            self.switch_renderer(backend);
        } else if !open {
            self.renderer_picker = None;
        }
    }

    fn switch_renderer(&mut self, backend: renderer_choice::Backend) {
        let backend = backend.safe_for_platform();
        if backend == self.renderer
            || self.running
            || !self.auth_pending.is_empty()
            || self.agent.as_ref().is_some_and(|a| a.try_lock().is_err())
            || self.exit.is_some()
        {
            return;
        }
        self.abort();
        let handoff = lifecycle::Handoff {
            token: feather_core::provider::gen_id("restart"),
            session: self.session_dir.clone(),
            draft: self.input.clone(),
            show_thinking: self.show_thinking,
            expand_tools: self.expand_tools,
        };
        self.exit = Some(lifecycle::Exit::restart(
            self.home.clone(),
            self.root.clone(),
            backend,
            handoff,
        ));
        self.egui_ctx.request_repaint();
    }

    fn poll_exit(&mut self, ctx: &egui::Context) {
        let busy = self.running
            || self
                .agent
                .as_ref()
                .is_some_and(|agent| agent.try_lock().is_err());
        let result = self.exit.as_mut().map(|exit| exit.poll(busy));
        match result {
            Some(Ok(true)) => {
                self.exit_ready = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            Some(Ok(false)) => {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                ctx.request_repaint_after(std::time::Duration::from_millis(25));
            }
            Some(Err(error)) => {
                lifecycle::cancel_pending();
                self.exit = None;
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                self.rows.push(ChatRow::new(
                    RowKind::Error,
                    format!("Renderer restart failed: {error}"),
                ));
            }
            None => {}
        }
    }

    fn shutdown_ui(&mut self, ctx: &egui::Context) {
        // No sync config/database reads, transcript shaping, modal saves, or
        // keyboard actions once Close/Restart was accepted. Keep the event loop
        // responsive while cooperative agent cancellation completes.
        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(PANEL))
            .show(ctx, |ui| {
                ui.vertical_centered(|ui| {
                    ui.add_space((ui.available_height() * 0.4).max(0.0));
                    ui.spinner();
                    ui.label(if self.exit.as_ref().and_then(|e| e.backend).is_some() {
                        "Restarting Feather…"
                    } else {
                        "Closing Feather…"
                    });
                });
            });
        ctx.request_repaint_after(std::time::Duration::from_millis(25));
    }

    fn abort(&mut self) {
        let _ = self.abort_tx.send(true);
    }

    // -- model / thinking --------------------------------------------------

    fn select_model(&mut self, provider_id: &str, model_id: &str) {
        if self.running {
            return;
        }
        self.settings.model = format!("{provider_id}/{model_id}");
        self.apply_settings_change();
    }

    fn toggle_models(&mut self) {
        self.show_models = !self.show_models;
        if self.show_models {
            self.egui_ctx.memory_mut(|m| {
                m.surrender_focus(egui::Id::new("feather-composer"));
                m.request_focus(egui::Id::new("model-search"));
            });
            self.show_login = false;
            self.show_settings = false;
            self.show_thinking_picker = false;
            self.renderer_picker = None;
            self.request_picker = None;
            self.picker = None;
            self.auth = oauth::AuthStore::load();
            self.model_filter.clear();
            self.model_cursor = 0;
        }
    }

    fn provider_available(&self, pid: &str, p: &models::ProviderEntry) -> bool {
        // Keep custom providers; hide built-in login providers without a usable
        // stored credential or configured API key. Refreshable expiry is allowed.
        oauth::provider(pid).is_none()
            || feather::auth::credential_available(&self.auth, pid, p.api_key.as_deref())
    }

    fn effort_levels(&self) -> Vec<String> {
        self.catalog
            .as_ref()
            .and_then(|cat| {
                let (pid, id) = self.settings.model.split_once('/')?;
                let provider = cat.providers.get(pid)?;
                let model = provider.models.iter().find(|m| m.id == id)?;
                Some(models::thinking_levels(model, provider))
            })
            .filter(|levels| !levels.is_empty())
            .unwrap_or_else(|| {
                models::DEFAULT_EFFORTS
                    .iter()
                    .map(|s| s.to_string())
                    .collect()
            })
    }

    fn open_thinking_picker(&mut self) {
        if self.running {
            return;
        }
        let cur = self.current_thinking();
        self.thinking_slider = self
            .effort_levels()
            .iter()
            .position(|l| l == cur)
            .unwrap_or(0);
        self.thinking_applied = self.thinking_slider;
        self.show_thinking_picker = !self.show_thinking_picker;
        if self.show_thinking_picker {
            self.renderer_picker = None;
            self.request_picker = None;
            self.egui_ctx
                .memory_mut(|m| m.surrender_focus(egui::Id::new("feather-composer")));
            self.show_models = false;
            self.show_login = false;
            self.show_settings = false;
            self.picker = None;
        }
    }

    fn cycle_thinking(&mut self, forward: bool) {
        let cur = self.current_thinking().to_string();
        let levels = self.effort_levels();
        let pos = levels.iter().position(|l| *l == cur).unwrap_or(0usize);
        let next = if forward {
            (pos + 1) % levels.len()
        } else {
            (pos + levels.len() - 1) % levels.len()
        };
        self.set_thinking(&levels[next]);
    }

    fn current_thinking(&self) -> &str {
        if self
            .catalog
            .as_ref()
            .and_then(|c| models::find(c, &self.settings.model))
            .map(|(_, _, m)| m.reasoning_not_supported)
            .unwrap_or(false)
        {
            return "off";
        }
        self.settings
            .model_thinking_levels
            .as_ref()
            .and_then(|m| m.get(&self.settings.model))
            .map(|s| s.as_str())
            .or(self.settings.default_thinking_level.as_deref())
            .unwrap_or("off")
    }

    fn set_thinking(&mut self, level: &str) {
        if self.running {
            return;
        }
        let model = self.settings.model.clone();
        let map = self
            .settings
            .model_thinking_levels
            .get_or_insert_with(Default::default);
        map.insert(model, level.to_string());
        self.apply_settings_change();
    }

    fn apply_settings_change(&mut self) {
        self.settings.save_default_if_missing();
        if let Err(e) = self.settings.save() {
            self.rows
                .push(ChatRow::new(RowKind::Error, format!("settings save: {e}")));
        }
        self.rebuild_agent(false);
    }

    // -- actions (keybindings) ---------------------------------------------

    fn handle_action(&mut self, action: &str, ctx: &egui::Context) {
        match action {
            "app.interrupt" | "tui.select.cancel" | "tui.altScreen.searchClose" => {
                if self.show_models
                    || self.show_settings
                    || self.show_login
                    || self.show_thinking_picker
                    || self.renderer_picker.is_some()
                    || self.request_picker.is_some()
                    || self.picker.is_some()
                {
                    self.show_models = false;
                    self.show_settings = false;
                    self.show_login = false;
                    self.show_thinking_picker = false;
                    self.renderer_picker = None;
                    self.request_picker = None;
                    self.picker = None;
                } else if self.running {
                    self.abort();
                }
            }
            "app.session.new" if !self.running => self.start_new_session(),
            "app.session.resume" if !self.running => self.open_session_picker(),
            "app.model.select" => self.toggle_models(),
            "app.model.cycleForward" => self.cycle_catalog_model(true),
            "app.model.cycleBackward" => self.cycle_catalog_model(false),
            "app.thinking.cycle" => self.cycle_thinking(true),
            "app.thinking.save" | "app.models.save" => {
                self.settings.save_default_if_missing();
                let _ = self.settings.save();
            }
            "app.thinking.select" => self.open_thinking_picker(),
            "app.shortcuts" => self.show_settings = !self.show_settings,
            "app.thinking.toggle" => self.show_thinking = !self.show_thinking,
            "app.tools.expand" => {
                self.expand_tools = !self.expand_tools;
                for row in &mut self.rows {
                    if row.kind == RowKind::Tool {
                        row.expanded = self.expand_tools;
                    }
                }
            }
            "app.message.followUp" => {
                // queue the current draft while running (pi parity)
                let text = self.input.trim().to_string();
                if !text.is_empty() && self.running {
                    self.input_queue
                        .push(feather_core::message::Message::user_text(text), false);
                    self.input.clear();
                    self.queued_hint += 1;
                }
            }
            "app.exit" => {
                if self.input.is_empty() && !self.running {
                    self.request_close();
                }
            }
            "tui.input.submit"
                if !self.show_login
                    && !self.show_models
                    && !self.show_settings
                    && !self.show_thinking_picker
                    && self.renderer_picker.is_none()
                    && self.request_picker.is_none()
                    && ctx.memory(|m| m.has_focus(egui::Id::new("feather-composer"))) =>
            {
                self.submit()
            }
            "tui.input.copy" => {} // egui handles selection copy natively
            // editor/list actions are handled natively by egui widgets; the
            // registry still carries them for the settings panel & overrides
            _ => {}
        }
    }

    fn cycle_catalog_model(&mut self, forward: bool) {
        let Some(cat) = self.catalog.as_ref() else {
            return;
        };
        let flat: Vec<(String, String)> = cat
            .providers
            .iter()
            .filter(|(pid, p)| self.provider_available(pid, p))
            .flat_map(|(pid, p)| p.models.iter().map(move |m| (pid.clone(), m.id.clone())))
            .collect();
        if flat.is_empty() {
            return;
        }
        let pos = flat
            .iter()
            .position(|(pid, id)| format!("{pid}/{id}") == self.settings.model)
            .unwrap_or(0);
        let next = if forward {
            (pos + 1) % flat.len()
        } else {
            (pos + flat.len() - 1) % flat.len()
        };
        let (pid, id) = flat[next].clone();
        self.select_model(&pid, &id);
    }

    // -- event drain -------------------------------------------------------

    fn drain_events(&mut self) {
        let started = std::time::Instant::now();
        let mut count = 0;
        while let Ok(ev) = self.ev_rx.try_recv() {
            match ev {
                UiEvent::Agent(agent_ev) => self.on_agent_event(agent_ev),
                UiEvent::RunDone(res) => {
                    for row in &mut self.rows {
                        row.streaming = false;
                    }
                    self.running = false;
                    let now = self.now();
                    self.metrics.finish_run(now);
                    match res {
                        Ok(r) => {
                            self.history.durable_tail_changed();
                            self.rewind_turns.invalidate();
                            self.last_stop = Some(r.stop_reason);
                            self.last_usage = r.usage;
                            if let Some(last) = self.rows.last_mut() {
                                last.streaming = false;
                            }
                            // Close the acceptance/completion race: a follow-up
                            // entered after core's final inbox check still runs.
                            if self.exit.is_none()
                                && !matches!(r.stop_reason, StopReason::Aborted | StopReason::Error)
                            {
                                if let Some(message) = self.input_queue.pop_follow_up() {
                                    self.queued_hint = self.queued_hint.saturating_sub(1);
                                    let draft = std::mem::replace(&mut self.input, message.text());
                                    self.submit();
                                    self.input = draft;
                                }
                            }
                        }
                        Err(e) => {
                            self.rows.push(ChatRow::new(RowKind::Error, e));
                        }
                    }
                }
                UiEvent::Auth {
                    provider,
                    action,
                    result,
                } => {
                    self.auth_pending.remove(provider);
                    match result {
                        Ok(()) => {
                            if self.exit.is_none() {
                                self.auth = oauth::AuthStore::load();
                            }
                            self.auth_changed = true;
                            self.api_key_drafts.remove(provider);
                            self.rows.push(ChatRow::new(
                                RowKind::Notice,
                                format!("{provider}: {action}"),
                            ));
                        }
                        Err(e) => {
                            self.rows
                                .push(ChatRow::new(RowKind::Error, format!("{provider}: {e}")));
                        }
                    }
                }
                UiEvent::ModelsRefreshed => {
                    // A discovered model list is cached in config.redb and the
                    // resolved catalog is rebuilt from it on the next read.
                    self.catalog = models::load();
                }
            }
            count += 1;
            if count >= 2048 || started.elapsed() >= std::time::Duration::from_millis(4) {
                self.egui_ctx
                    .request_repaint_after(std::time::Duration::from_millis(33));
                break;
            }
        }
    }

    fn on_agent_event(&mut self, ev: AgentEvent) {
        let now = self.now();
        match ev {
            AgentEvent::AuditCommitted { raw_before } => {
                self.history.committed(raw_before, &self.rows);
                self.rewind_turns.invalidate();
            }
            AgentEvent::TurnStart(_) => {
                // A request boundary must never reuse an earlier response row,
                // even if a frontend missed its finalize event while compacting.
                for row in &mut self.rows {
                    if row.kind == RowKind::Assistant {
                        row.streaming = false;
                    }
                }
                self.metrics.begin_turn(now);
            }
            AgentEvent::ResponseReset {
                attempt,
                max_attempts,
                delay_ms,
                reason,
            } => {
                // Only the current uncommitted response is discarded. Original
                // audit rows and completed tool results never participate.
                if let Some(row) = self
                    .rows
                    .iter_mut()
                    .rev()
                    .find(|r| r.kind == RowKind::Assistant && r.streaming)
                {
                    row.text.clear();
                    row.thinking.clear();
                }
                self.metrics.begin_turn(now);
                self.rows.push(ChatRow::new(
                    RowKind::Notice,
                    format!(
                        "Retry {}/{} in {}s · {reason}",
                        attempt - 1,
                        max_attempts - 1,
                        delay_ms / 1000
                    ),
                ));
            }
            AgentEvent::TextDelta(t) => {
                self.metrics.record_delta(&t, now);
                self.ensure_streaming_row().text.push_str(&t);
            }
            AgentEvent::ThinkingDelta(t) => {
                self.metrics.record_delta(&t, now);
                self.ensure_streaming_row().thinking.push_str(&t);
            }
            AgentEvent::AssistantMessage(m) => {
                if m.role == feather_core::message::Role::User {
                    // Core emits queued prompts only on delivery, after the
                    // current final assistant message has been persisted.
                    self.rows.push(ChatRow::new(RowKind::User, m.text()));
                    self.queued_hint = self.queued_hint.saturating_sub(1);
                    return;
                }
                let tokens = m.usage.map(|u| u.output).unwrap_or(0);
                self.metrics.finish_turn(tokens, now);
                if let Some(row) = self
                    .rows
                    .iter_mut()
                    .rev()
                    .find(|r| r.kind == RowKind::Assistant && r.streaming)
                {
                    // Final message replaces the live row, never duplicates it.
                    row.text = m.text();
                    row.thinking = m.thinking();
                    row.streaming = false;
                } else if !m.text().is_empty() || !m.thinking().is_empty() {
                    let mut row = ChatRow::new(RowKind::Assistant, m.text());
                    row.thinking = m.thinking();
                    self.rows.push(row);
                }
                self.history.durable_tail_changed();
            }
            AgentEvent::ToolStart { call } => {
                for row in &mut self.rows {
                    if row.kind == RowKind::Assistant {
                        row.streaming = false;
                    }
                }
                let mut row = ChatRow::new(RowKind::Tool, &call.name);
                row.tool_args = tool_summary(&call.arguments);
                row.expanded = self.expand_tools;
                row.tool_id = Some(call.id);
                row.started_at = Some(now);
                row.streaming = true;
                self.rows.push(row);
            }
            AgentEvent::ToolEnd {
                call,
                text,
                is_error,
            } => {
                let text = truncate_utf8(&text, 20_000);
                if let Some(row) = self
                    .rows
                    .iter_mut()
                    .rev()
                    .find(|r| r.tool_id.as_deref() == Some(&call.id))
                {
                    row.tool_output = Some(text);
                    row.tool_error = is_error;
                    row.streaming = false;
                    row.elapsed = row.started_at.map(|start| (now - start).max(0.0));
                } else {
                    let mut row = ChatRow::new(RowKind::Tool, &call.name);
                    row.tool_id = Some(call.id);
                    row.tool_args = tool_summary(&call.arguments);
                    row.tool_output = Some(text);
                    row.tool_error = is_error;
                    self.rows.push(row);
                }
                self.history.durable_tail_changed();
            }
            AgentEvent::ToolProgress { call, text } => {
                let text = truncate_utf8(&text, 20_000);
                // Live progress replaces the output body but never ends the
                // run: `streaming` stays true until ToolEnd, so the spinner
                // keeps moving and the row is not finalized early.
                if let Some(row) = self
                    .rows
                    .iter_mut()
                    .rev()
                    .find(|r| r.tool_id.as_deref() == Some(&call.id))
                {
                    row.tool_output = Some(text);
                }
            }
            AgentEvent::Notice(n) => self.rows.push(ChatRow::new(RowKind::Notice, n)),
            AgentEvent::Error(e) => self.rows.push(ChatRow::new(RowKind::Error, e)),
            AgentEvent::CompactionDone {
                tokens_before,
                tokens_after,
                chars_saved,
            } => {
                if chars_saved > 0 && tokens_after < tokens_before {
                    self.rows.push(ChatRow::new(RowKind::Notice,
                        format!("Context compacted · {tokens_before} → {tokens_after} estimated tokens · {chars_saved} characters saved")));
                }
            }
            AgentEvent::TurnEnd(_) => {}
            AgentEvent::RunEnd(r) => {
                // This inner run may be followed by queued prompts; only
                // ordered RunDone marks the background prompt task idle.
                self.last_stop = Some(r.stop_reason);
                self.last_usage = r.usage;
                for row in &mut self.rows {
                    row.streaming = false;
                }
            }
        }
    }

    fn ensure_streaming_row(&mut self) -> &mut ChatRow {
        let index = self
            .rows
            .iter()
            .rposition(|r| r.kind == RowKind::Assistant && r.streaming)
            .unwrap_or_else(|| {
                let mut row = ChatRow::new(RowKind::Assistant, "");
                row.streaming = true;
                self.rows.push(row);
                self.rows.len() - 1
            });
        &mut self.rows[index]
    }

    // -- panels -------------------------------------------------------------

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        use window_chrome::Control;
        let (bar, _) =
            ui.allocate_exact_size(egui::vec2(ui.available_width(), 60.0), egui::Sense::hover());
        let right = bar.right() - 16.0;
        let button = |index: f32| {
            egui::Rect::from_min_size(
                egui::pos2(right - 36.0 - index * 42.0, bar.center().y - 17.0),
                egui::vec2(36.0, 34.0),
            )
        };
        let max = ui.input(|i| i.viewport().maximized.unwrap_or(false));
        let divider_x = button(2.0).left() - 22.0;
        let add_rect = egui::Rect::from_min_size(
            egui::pos2(divider_x - 122.0, bar.center().y - 17.0),
            egui::vec2(100.0, 34.0),
        );
        // The entire non-interactive header is draggable; controls are separate,
        // reserved rectangles and are never displaced by long workspace names.
        let drag_rect =
            egui::Rect::from_min_max(bar.min, egui::pos2(add_rect.left() - 8.0, bar.bottom()));
        let drag = ui.interact(
            drag_rect,
            ui.id().with("window-drag"),
            egui::Sense {
                click: true,
                drag: true,
                focusable: false,
            },
        );
        if drag.drag_started() {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
        }
        if drag.double_clicked() {
            ui.ctx()
                .send_viewport_cmd(egui::ViewportCommand::Maximized(!max));
        }
        if window_chrome::add_api(ui, add_rect).clicked() {
            self.renderer_picker = None;
            self.request_picker = None;
            self.show_login = true;
            self.model_manager.enabled = true;
            self.picker = None;
        }
        if window_chrome::control(ui, button(2.0), Control::Minimize).clicked() {
            ui.ctx()
                .send_viewport_cmd(egui::ViewportCommand::Minimized(true));
        }
        if window_chrome::control(
            ui,
            button(1.0),
            if max {
                Control::Restore
            } else {
                Control::Maximize
            },
        )
        .clicked()
        {
            ui.ctx()
                .send_viewport_cmd(egui::ViewportCommand::Maximized(!max));
        }
        if window_chrome::control(ui, button(0.0), Control::Close).clicked() {
            self.request_close();
        }
        ui.painter().line_segment(
            [
                egui::pos2(divider_x, bar.center().y - 8.0),
                egui::pos2(divider_x, bar.center().y + 8.0),
            ],
            egui::Stroke::new(1.2_f32, egui::Color32::from_rgb(170, 167, 158)),
        );
        let brand =
            egui::Rect::from_min_size(bar.min + egui::vec2(18.0, 12.0), egui::vec2(136.0, 36.0));
        ui.allocate_new_ui(
            egui::UiBuilder::new()
                .max_rect(brand)
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
            |ui| {
                ui.set_clip_rect(brand);
                theme::mascot(ui, 32.0);
                ui.label(
                    egui::RichText::new("Feather")
                        .size(20.0)
                        .strong()
                        .color(INK),
                );
            },
        );
        let path_rect = egui::Rect::from_min_max(
            egui::pos2(brand.right() + 16.0, bar.top() + 17.0),
            egui::pos2(
                (add_rect.left() - 24.0).max(brand.right() + 16.0),
                bar.bottom() - 17.0,
            ),
        );
        ui.allocate_new_ui(
            egui::UiBuilder::new()
                .max_rect(path_rect)
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
            |ui| {
                ui.set_clip_rect(path_rect);
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(short_path(&self.root))
                            .size(12.5)
                            .color(MUTED),
                    )
                    .truncate(),
                )
                .on_hover_text(self.root.display().to_string());
            },
        );
    }

    /// Left panel: the working directory tree (lazy: dirs are read on expand;
    /// clicking a file inserts an @path reference into the composer).
    fn tree_panel(&mut self, ctx: &egui::Context) {
        egui::SidePanel::left("feather_tree")
            .show_separator_line(false)
            .resizable(true)
            .default_width(204.0)
            .width_range(170.0..=280.0)
            .frame(
                egui::Frame::none()
                    .fill(PANEL)
                    .stroke(egui::Stroke::NONE)
                    .inner_margin(egui::Margin {
                        left: 14.0,
                        right: 4.0,
                        top: 4.0,
                        bottom: 12.0,
                    }),
            )
            .show(ctx, |ui| {
                egui::Frame::none()
                    .fill(SIDEBAR)
                    .rounding(22.0)
                    .inner_margin(egui::Margin::symmetric(14.0, 18.0))
                    .show(ui, |ui| {
                        ui.set_min_height((ui.available_height() - 4.0).max(0.0));
                        ui.label(
                            egui::RichText::new("WORKSPACE")
                                .size(11.5)
                                .strong()
                                .color(MUTED),
                        );
                        ui.add_space(3.0);
                        ui.label(
                            egui::RichText::new(
                                self.root.file_name().unwrap_or_default().to_string_lossy(),
                            )
                            .size(16.0)
                            .strong()
                            .color(TEXT),
                        );
                        ui.add_space(10.0);
                        ui.spacing_mut().item_spacing.y = 3.0;
                        egui::ScrollArea::vertical()
                            .id_salt("workspace-tree")
                            .auto_shrink([false, false])
                            .scroll_bar_visibility(
                                egui::scroll_area::ScrollBarVisibility::AlwaysVisible,
                            )
                            .show(ui, |ui| {
                                // Scope the registry-style rails to the workspace tree;
                                // other collapsing panels keep their lighter appearance.
                                ui.visuals_mut().indent_has_left_vline = true;
                                ui.visuals_mut().widgets.noninteractive.bg_stroke =
                                    egui::Stroke::new(1.0_f32, workspace::TREE_GUIDE_COLOR);
                                // Folder bodies are indented; egui paints their left rails using this local stroke.
                                let root = self.root.clone();
                                self.tree_dir(ui, &root, 0);
                            });
                    });
            });
    }

    fn tree_dir(&mut self, ui: &mut egui::Ui, dir: &std::path::Path, depth: u32) {
        if depth > 8 {
            return;
        }
        let listing = workspace::listing(ui.ctx(), dir);
        let dirs = &listing.dirs;
        let files = &listing.files;

        for name in dirs {
            let path = dir.join(name);
            // CollapsingHeader normally extends long labels. Give it a single-line
            // galley instead so deep/long folder names cannot widen the sidebar.
            let header_width =
                (ui.available_width() - ui.spacing().indent - ui.spacing().button_padding.x)
                    .max(0.0);
            let header = egui::WidgetText::from(egui::RichText::new(name).size(13.0).color(TEXT))
                .into_galley(
                    ui,
                    Some(egui::TextWrapMode::Truncate),
                    header_width,
                    egui::TextStyle::Button,
                );

            ui.push_id(path.to_string_lossy().to_string(), |ui| {
                egui::CollapsingHeader::new(header)
                    .default_open(depth == 0 && dirs.len() <= 6)
                    // Render the normal indented body so egui paints a vertical guide rail.
                    .show(ui, |ui| {
                        self.tree_dir(ui, &path, depth + 1);
                    })
                    .header_response
                    .on_hover_text(path.display().to_string());
            });
        }
        for name in files {
            let path = dir.join(name);
            let rel = path
                .strip_prefix(&self.root)
                .unwrap_or(&path)
                .display()
                .to_string();
            // Reserve the same disclosure-arrow column as folder headers, so
            // files and folders belonging to one parent share a text baseline.
            let response = ui
                .allocate_ui_with_layout(
                    egui::vec2(ui.available_width().max(0.0), 20.0),
                    egui::Layout::left_to_right(egui::Align::Center),
                    |ui| {
                        ui.spacing_mut().item_spacing.x = 0.0;
                        ui.add_space(ui.spacing().indent);
                        ui.add_sized(
                            [ui.available_width().max(0.0), 20.0],
                            egui::Label::new(egui::RichText::new(name).size(13.0).color(TEXT))
                                .halign(egui::Align::Min)
                                .truncate()
                                .sense(egui::Sense::click()),
                        )
                    },
                )
                .inner;
            if response
                .on_hover_cursor(egui::CursorIcon::PointingHand)
                .on_hover_text(name)
                .clicked()
            {
                self.input.push_str(&format!("@{rel} "));
            }
        }
        if dirs.len() == 200 || files.len() == 300 {
            ui.label(egui::RichText::new("…").size(11.0).color(MUTED));
        }
    }

    fn chat_panel(&mut self, ui: &mut egui::Ui) {
        if self.rows.is_empty() {
            let rect = ui.available_rect_before_wrap();
            ui.allocate_new_ui(
                egui::UiBuilder::new().max_rect(egui::Rect::from_center_size(
                    egui::pos2(rect.center().x, rect.top() + rect.height() * 0.43),
                    egui::vec2(rect.width().min(500.0), 270.0),
                )),
                |ui| {
                    ui.vertical_centered(|ui| {
                        theme::mascot(ui, 146.0);
                        ui.add_space(8.0);
                        ui.label(
                            egui::RichText::new("A lighter place to build.")
                                .size(26.0)
                                .strong()
                                .color(INK),
                        );
                        ui.add_space(7.0);
                        ui.label(
                            egui::RichText::new("Bring an idea. Feather will take it from here.")
                                .size(15.0)
                                .color(MUTED),
                        );
                        ui.add_space(20.0);
                        ui.horizontal_wrapped(|ui| {
                            for (key, label) in [
                                ("Ctrl+L", "Models"),
                                ("Shift+Tab", "Thinking"),
                                ("F1", "Shortcuts"),
                            ] {
                                egui::Frame::none()
                                    .fill(SOFT_ACCENT)
                                    .rounding(8.0)
                                    .inner_margin(egui::Margin::symmetric(9.0, 5.0))
                                    .show(ui, |ui| {
                                        ui.horizontal(|ui| {
                                            ui.label(
                                                egui::RichText::new(key)
                                                    .size(11.5)
                                                    .strong()
                                                    .color(INK),
                                            );
                                            ui.label(
                                                egui::RichText::new(label).size(11.5).color(MUTED),
                                            );
                                        });
                                    });
                            }
                        });
                    });
                },
            );
            return;
        }
        let now = self.now();
        if self.history.paged() {
            ui.horizontal(|ui| {
                if self.history.loading() {
                    ui.spinner();
                }
                if ui
                    .add_enabled(
                        self.history.remaining() > 0,
                        egui::Link::new(if self.history.loading() {
                            "Loading earlier conversation…".into()
                        } else if self.history.remaining() == 0 {
                            "Beginning of conversation".into()
                        } else {
                            format!(
                                "Load earlier conversation · {} entries",
                                self.history.remaining()
                            )
                        }),
                    )
                    .clicked()
                {
                    self.history.request_older(ui.ctx());
                }
                if let Some(error) = &self.history.error {
                    ui.label(egui::RichText::new(error).small().color(RED));
                }
            });
        }
        transcript::show(ui, &mut self.rows, now, self.show_thinking, self.running);
        let viewport = transcript::view(ui.ctx());
        self.history
            .bound_live_tail(&self.rows, viewport.at_bottom, ui.ctx());
        if !self.history.loading() && (viewport.scrolling_down || viewport.at_bottom) {
            if self.history.evict_above(
                &mut self.rows,
                viewport.first_visible_key,
                viewport.at_bottom,
            ) > 0
            {
                cards::release_history_layouts(ui.ctx());
                ui.ctx()
                    .request_discard("older redb page released; preserve viewport anchor");
            }
        }
        if viewport.near_top && self.history.remaining() > 0 && self.history.error.is_none() {
            self.history.request_older(ui.ctx());
        }
    }

    fn input_bar(&mut self, ui: &mut egui::Ui) {
        // Intentional conversation/composer divider. The opaque GPU canvas
        // eliminates the accidental antialiased panel-edge seam separately.
        let boundary = ui.available_rect_before_wrap();
        let y = ui.painter().round_to_pixel_center(boundary.top());
        ui.painter().line_segment(
            [
                egui::pos2(boundary.left(), y),
                egui::pos2(boundary.right(), y),
            ],
            egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(209, 205, 196)),
        );
        ui.add_space(8.0);
        let width = (theme::content_width(ui.available_width()) + 30.0).min(ui.available_width());
        let margin = ((ui.available_width() - width) * 0.5).max(0.0);
        ui.horizontal_top(|ui| {
            ui.add_space(margin);
            ui.vertical(|ui| {
                ui.set_width(width);
                let options = self.cached_suggestions();
                if !options.is_empty()
                    && !self.show_login
                    && !self.show_models
                    && self.picker.is_none()
                {
                    let mut selected = None;
                    let mut cursor = self.command_cursor;
                    egui::Frame::none()
                        .fill(SOFT_ACCENT)
                        .rounding(12.0)
                        .inner_margin(10.0)
                        .show(ui, |ui| {
                            ui.small(if skill_choices::query(&self.input).is_some() {
                                "Skills · ↑ / ↓ choose · Tab / Enter insert"
                            } else if rewind_choices::query(&self.input).is_some() {
                                "Turns · ↑ / ↓ choose · Enter rewinds"
                            } else {
                                "Commands · ↑ / ↓ choose · Tab / Enter select"
                            });
                            egui::ScrollArea::vertical()
                                .id_salt("slash-commands")
                                .max_height(230.0)
                                .show(ui, |ui| {
                                    for (index, c) in options.iter().enumerate() {
                                        let selected_row = index == cursor;
                                        let response = egui::Frame::none()
                                            .fill(if selected_row {
                                                theme::USER_BUBBLE
                                            } else {
                                                PANEL_2
                                            })
                                            .rounding(10.0)
                                            .inner_margin(8.0)
                                            .show(ui, |ui| {
                                                ui.set_width(ui.available_width());
                                                ui.horizontal(|ui| {
                                                    ui.label(egui::RichText::new(&c.0).color(TEAL));
                                                    ui.with_layout(
                                                        egui::Layout::right_to_left(
                                                            egui::Align::Center,
                                                        ),
                                                        |ui| {
                                                            ui.add(
                                                                egui::Label::new(
                                                                    egui::RichText::new(&c.1)
                                                                        .small(),
                                                                )
                                                                .truncate(),
                                                            );
                                                        },
                                                    );
                                                });
                                            })
                                            .response;
                                        let hit = ui.interact(
                                            response.rect,
                                            ui.id().with(index),
                                            egui::Sense::click(),
                                        );
                                        if hit.hovered() {
                                            if ui.input(|i| {
                                                i.events.iter().any(|e| {
                                                    matches!(e, egui::Event::PointerMoved(_))
                                                })
                                            }) {
                                                cursor = index;
                                            }
                                            ui.painter().rect_stroke(
                                                response.rect,
                                                10.0,
                                                egui::Stroke::new(1.5_f32, TEAL),
                                            );
                                        }
                                        if hit.clicked() {
                                            selected = Some(c.clone());
                                        }
                                        if selected_row
                                            && ui.input(|i| {
                                                i.key_pressed(egui::Key::ArrowUp)
                                                    || i.key_pressed(egui::Key::ArrowDown)
                                            })
                                        {
                                            response.scroll_to_me(Some(egui::Align::Center));
                                        }
                                    }
                                });
                        });
                    drop(options);
                    self.command_cursor = cursor;
                    if let Some(c) = selected {
                        self.accept_suggestion(c, ui.ctx());
                    }
                } else if options.is_empty()
                    && !self.show_login
                    && !self.show_models
                    && self.picker.is_none()
                {
                    drop(options);
                    if let Some(status) = self.suggestion_status() {
                        egui::Frame::none()
                            .fill(SOFT_ACCENT)
                            .rounding(12.0)
                            .inner_margin(10.0)
                            .show(ui, |ui| {
                                ui.label(egui::RichText::new(status).small().color(MUTED));
                                if skill_choices::query(&self.input).is_some() {
                                    for diagnostic in
                                        self.skill_choices.resources.diagnostics.iter().take(2)
                                    {
                                        ui.label(
                                            egui::RichText::new(&diagnostic.message)
                                                .small()
                                                .color(RED),
                                        );
                                    }
                                }
                            });
                    }
                } else {
                    drop(options);
                }
                egui::Frame::none()
                    .fill(PANEL_2)
                    .rounding(22.0)
                    .shadow(soft_shadow())
                    .inner_margin(egui::Margin::symmetric(19.0, 15.0))
                    .show(ui, |ui| {
                        let id = egui::Id::new("feather-composer");
                        // Submit keys are consumed by the named-action registry
                        // before TextEdit sees them; overrides remain authoritative.
                        // This scroll area owns caret scroll requests; they must not
                        // leak into the transcript rendered after the bottom panel.
                        let response = egui::ScrollArea::vertical()
                            .id_salt("composer-scroll")
                            .max_height(84.0)
                            .min_scrolled_height(48.0)
                            .auto_shrink([false, true])
                            .animated(false)
                            .show(ui, |ui| {
                                ui.add(
                                    egui::TextEdit::multiline(&mut self.input)
                                        .id(id)
                                        .font(egui::FontId::proportional(15.5))
                                        .text_color(TEXT)
                                        .hint_text(
                                            egui::RichText::new("What's on your mind?")
                                                .color(MUTED),
                                        )
                                        .desired_rows(2)
                                        .desired_width(ui.available_width())
                                        .frame(false),
                                )
                            })
                            .inner;
                        if !self.show_login
                            && !self.show_models
                            && !self.show_settings
                            && !self.show_thinking_picker
                            && self.renderer_picker.is_none()
                            && self.request_picker.is_none()
                            && self.picker.is_none()
                        {
                            if !ui.ctx().wants_keyboard_input() {
                                response.request_focus();
                            }
                        }
                        ui.add_space(5.0);
                        ui.horizontal(|ui| {
                            let hint = if self.queued_hint > 0 {
                                format!(
                                    "{} queued follow-up(s) · Enter queues · Esc stops",
                                    self.queued_hint
                                )
                            } else if self.running {
                                "Enter queues a follow-up · Esc stops".into()
                            } else {
                                "Enter to send · Shift+Enter for a new line · @ to reference a file"
                                    .into()
                            };
                            let (hint_rect, _) = ui.allocate_exact_size(
                                egui::vec2(ui.available_width().max(40.0), 22.0),
                                egui::Sense::hover(),
                            );
                            let galley = ui.fonts(|f| {
                                let mut job = egui::text::LayoutJob::simple(
                                    hint,
                                    egui::FontId::proportional(11.5),
                                    MUTED,
                                    hint_rect.width(),
                                );
                                job.wrap.max_rows = 1;
                                job.wrap.break_anywhere = true;
                                f.layout_job(job)
                            });
                            ui.painter().galley(
                                egui::pos2(
                                    hint_rect.left(),
                                    hint_rect.center().y - galley.size().y * 0.5,
                                ),
                                galley,
                                MUTED,
                            );
                        });
                    });
                ui.add_space(10.0);
                self.status_bar(ui);
                ui.add_space(16.0);
            });
        });
    }

    fn suggestion_status(&self) -> Option<String> {
        if skill_choices::query(&self.input).is_some() {
            return Some(if !self.skill_choices.loaded {
                "Loading skills…".into()
            } else if self.skill_choices.resources.skills.is_empty() {
                format!(
                    "No usable skills found in {} or {}.",
                    self.home.join("skills").display(),
                    self.root.join(".feather/skills").display()
                )
            } else {
                "No skills match your search.".into()
            });
        }
        if rewind_choices::query(&self.input).is_some() {
            return Some(if let Some(error) = &self.rewind_turns.error {
                format!("Cannot load turns: {error}")
            } else if !self.rewind_turns.loaded {
                "Loading committed turns…".into()
            } else if self.agent.is_none() && !self.preview {
                "No active session. Resume a saved session to choose a turn.".into()
            } else if self.rewind_turns.rows.is_empty() {
                "No committed turns yet. Turns appear after a request is saved.".into()
            } else {
                "No turns match your search.".into()
            });
        }
        None
    }
    fn accept_suggestion(&mut self, choice: (String, String, bool), ctx: &egui::Context) {
        self.input = choice.0;
        self.command_cursor = 0;
        if choice.2 {
            self.submit();
        }
        let id = egui::Id::new("feather-composer");
        let mut state = egui::TextEdit::load_state(ctx, id).unwrap_or_default();
        state
            .cursor
            .set_char_range(Some(egui::text::CCursorRange::one(
                egui::text::CCursor::new(self.input.chars().count()),
            )));
        state.store(ctx, id);
        ctx.memory_mut(|m| m.request_focus(id));
    }

    fn cached_suggestions(&self) -> std::cell::Ref<'_, [suggestion_cache::Suggestion]> {
        let menu_open = skill_choices::query(&self.input).is_some()
            || rewind_choices::query(&self.input).is_some()
            || (self.input.starts_with('/') && !self.input.chars().any(char::is_whitespace));
        let cache_key = if menu_open { self.input.as_str() } else { "" };
        self.suggestions.get(
            cache_key,
            self.skill_choices.revision,
            self.rewind_turns.revision,
            || self.input_suggestions(),
        )
    }
    fn input_suggestions(&self) -> Vec<(String, String, bool)> {
        if skill_choices::query(&self.input).is_some() {
            return self.skill_choices.suggestions(&self.input);
        }
        if rewind_choices::query(&self.input).is_some() {
            return self
                .rewind_turns
                .suggestions(&self.input)
                .into_iter()
                .map(|(a, b)| (a, b, true))
                .collect();
        }
        commands::suggestions(&self.input)
            .into_iter()
            .map(|c| {
                (
                    format!("/{}{}", c.name, if c.args { " " } else { "" }),
                    c.description.into(),
                    !c.args,
                )
            })
            .collect()
    }
    fn measure_status(ui: &egui::Ui, text: &str) -> f32 {
        ui.fonts(|fonts| {
            fonts.layout_no_wrap(
                text.to_owned(),
                egui::FontId::proportional(12.0),
                egui::Color32::WHITE,
            )
        })
        .size()
        .x
        .ceil()
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        let available = ui.available_width();
        let tight = available < 620.0;
        // Uniform rhythm: every item occupies only its measured (capped) width
        // and all items share one 7pt gap, so "a · b" is spaced identically on
        // both halves of the row. Fixed slots previously left dead air between
        // short values and their separator.
        const GAP: f32 = 7.0;
        let dot = Self::measure_status(ui, "·");
        let workspace = self
            .root
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let short_id = self.session_footer_label();
        let session_full = format!("{workspace} · {short_id}");
        let session_text = if tight {
            short_id.as_str()
        } else {
            session_full.as_str()
        };
        let model_full = self.settings.model.clone();
        let model_text = model_full
            .split_once('/')
            .map_or(model_full.as_str(), |(_, id)| id);
        let level = self.current_thinking();
        let effort_text = if tight {
            format!("effort {level}")
        } else {
            format!("thinking {level}")
        };
        let throughput = {
            let label = self.metrics.label(self.now());
            if label.is_empty() {
                "—".to_string()
            } else {
                label
            }
        };
        let tps_text = format!("TPS {throughput}");
        let proxy_text = self.proxies.label();
        let limit_text = self.limits.label();
        // Widths may only be reduced for overflow; never expanded beyond the
        // measured text, otherwise short values regain the old dead-air gaps.
        let session_natural = Self::measure_status(ui, &session_text);
        let model_natural = Self::measure_status(ui, &model_text);
        let mut session_w = session_natural.min(if tight { 150.0 } else { 300.0 });
        let mut model_w = model_natural.min(if tight { 150.0 } else { 220.0 });
        let effort_w = Self::measure_status(ui, &effort_text).min(128.0);
        // Telemetry is the only purely informational footer item, so at narrow
        // widths it yields its space to keep every control visible and
        // un-truncated (truncation also breaks the uniform separator rhythm).
        let show_tps = !tight;
        let tps_w = if show_tps {
            Self::measure_status(ui, &tps_text).min(120.0)
        } else {
            0.0
        };
        let render_w = Self::measure_status(ui, self.renderer.status());
        let proxy_w = Self::measure_status(ui, &proxy_text).min(190.0);
        let limit_w = Self::measure_status(ui, &limit_text).min(150.0);
        let right_width = proxy_w + limit_w + render_w + dot * 2.0 + 3.0 * GAP;
        let gutter = 24.0;
        let left_width = (available - right_width - gutter).max(100.0);
        // The natural width follows what is actually rendered: without TPS
        // there is one fewer separator and two fewer gaps.
        let natural = if show_tps {
            session_w + model_w + effort_w + tps_w + 3.0 * dot + 6.0 * GAP
        } else {
            session_w + model_w + effort_w + 2.0 * dot + 4.0 * GAP
        };
        if natural > left_width {
            let identities = (session_w + model_w).max(1.0);
            let needed = (natural - left_width).min((identities - 40.0).max(0.0));
            let factor = (identities - needed) / identities;
            session_w = (session_w * factor).max(24.0).min(session_natural);
            model_w = (model_w * factor).max(24.0).min(model_natural);
        }
        ui.horizontal(|ui| {
            let rect=ui.available_rect_before_wrap();
            let y=ui.painter().round_to_pixel_center(rect.top()-3.0);
            ui.painter().line_segment([egui::pos2(rect.left(),y),egui::pos2(rect.right(),y)],egui::Stroke::new(1.0_f32,egui::Color32::from_rgb(229,224,216)));
            ui.set_min_height(28.0);ui.spacing_mut().item_spacing.x=0.0;
            ui.allocate_ui_with_layout(egui::vec2(left_width,28.0),egui::Layout::left_to_right(egui::Align::Center),|ui| {
                ui.set_min_width(left_width);
                ui.spacing_mut().item_spacing.x=GAP;
                if ui.add_sized([session_w,22.0],egui::Label::new(egui::RichText::new(session_text).size(12.0).color(TEAL)).halign(egui::Align::Min).truncate().sense(egui::Sense::click()))
                    .on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text(format!("{session_full}\nWorkspace sessions · Ctrl+Shift+R")).clicked(){self.open_session_picker();}
                ui.label(egui::RichText::new("·").size(12.0).color(MUTED));
                if ui.add_sized([model_w,22.0],egui::Label::new(egui::RichText::new(model_text).size(12.0).color(TEAL)).halign(egui::Align::Min).truncate().sense(egui::Sense::click()))
                    .on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text(format!("{}\nClick or Ctrl+L to choose a model",self.settings.model)).clicked(){self.toggle_models();}
                ui.label(egui::RichText::new("·").size(12.0).color(MUTED));
                if ui.add_sized([effort_w,22.0],egui::Label::new(egui::RichText::new(&effort_text).size(12.0).color(TEAL)).halign(egui::Align::Min).truncate().sense(egui::Sense::click()))
                    .on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text("Click to adjust effort · Ctrl+Shift+T · Shift+Tab cycles").clicked(){self.open_thinking_picker();}
                if show_tps {
                    ui.label(egui::RichText::new("·").size(12.0).color(MUTED));
                    ui.add_sized([tps_w,22.0],egui::Label::new(egui::RichText::new(&tps_text).size(12.0).color(INK)).halign(egui::Align::Min).truncate())
                        .on_hover_text("While streaming: ~ is an estimate. At completion: provider output tokens / generation time.");
                }
            });
            ui.add_space(gutter);
            ui.allocate_ui_with_layout(egui::vec2(right_width,28.0),egui::Layout::right_to_left(egui::Align::Center),|ui| {
                ui.spacing_mut().item_spacing.x=GAP;
                if ui.add_sized([proxy_w,22.0],egui::Label::new(egui::RichText::new(&proxy_text).size(12.0).color(TEAL)).halign(egui::Align::Min).truncate().sense(egui::Sense::click()))
                    .on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text("Choose or turn off proxy for Feather requests · applies to next request, no restart").clicked(){self.open_request_picker(false);}
                ui.label(egui::RichText::new("·").size(12.0).color(MUTED));
                let limit_hint = if self.limits.limit.bytes_per_second == 0 {
                    "Cap how much bandwidth Feather uses · currently unlimited".to_string()
                } else {
                    format!(
                        "Feather is limited to {}/s for model traffic\nClick to change or remove the limit",
                        feather::netlimit::human(self.limits.limit.bytes_per_second)
                    )
                };
                if ui.add_sized([limit_w,22.0],egui::Label::new(egui::RichText::new(&limit_text).size(12.0).color(if self.limits.limit.bytes_per_second==0{MUTED}else{TEAL})).halign(egui::Align::Min).truncate().sense(egui::Sense::click()))
                    .on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text(limit_hint).clicked(){self.open_rate_limit_picker();}
                ui.label(egui::RichText::new("·").size(12.0).color(MUTED));
                if renderer_choice::trigger(ui,self.renderer){self.open_renderer_picker();}
            });
        });
    }

    /// Startup/resume chooser modal. Closing it (×, Esc) or picking "new"
    /// returns to a fresh memory welcome without restoring any transcript.
    fn resume_window(&mut self, ctx: &egui::Context) {
        if self.picker.is_none() {
            return;
        }
        let mut open = true;
        let pick = {
            let chooser = self.picker.as_mut().expect("picker");
            sessions::picker_window(ctx, chooser, &mut open)
        };
        if let Some(pick) = pick {
            self.choose_pick(pick);
            return;
        }
        if !open {
            self.picker = None;
        }
    }

    /// Compact footer identity: the chosen session id, or `new` before one is
    /// materialized.
    fn session_footer_label(&self) -> String {
        if self.picker.is_some() {
            return "choosing".into();
        }
        match self.session_dir.file_name().and_then(|name| name.to_str()) {
            Some(id) if !id.is_empty() => sessions::short_id(id),
            _ => "new".into(),
        }
    }

    fn models_window(&mut self, ctx: &egui::Context) {
        if !self.show_models {
            return;
        }
        let mut picked = None;
        let mut open = self.show_models;
        let available: std::collections::BTreeSet<_> = self
            .catalog
            .as_ref()
            .map(|cat| {
                cat.providers
                    .iter()
                    .filter(|(pid, p)| self.provider_available(pid, p))
                    .map(|(pid, _)| pid.clone())
                    .collect()
            })
            .unwrap_or_default();

        // Everything the rows need is computed before the sheet's region
        // closures run, and every intent is recorded rather than applied while
        // the card is being drawn.
        let query = self.model_filter.to_lowercase();
        let flat: Vec<(String, String)> = self
            .catalog
            .as_ref()
            .map(|cat| {
                cat.providers
                    .iter()
                    .filter(|(pid, _)| available.contains(*pid))
                    .flat_map(|(pid, p)| p.models.iter().map(move |m| (pid.clone(), m.id.clone())))
                    .filter(|(pid, id)| {
                        query.is_empty() || format!("{pid}/{id}").to_lowercase().contains(&query)
                    })
                    .collect()
            })
            .unwrap_or_default();
        self.model_cursor = self.model_cursor.min(flat.len().saturating_sub(1));

        let (down, up, enter) = ctx.input_mut(|i| {
            (
                i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown),
                i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp),
                i.consume_key(egui::Modifiers::NONE, egui::Key::Enter),
            )
        });
        if !flat.is_empty() {
            if down {
                self.model_cursor = (self.model_cursor + 1) % flat.len();
            }
            if up {
                self.model_cursor = (self.model_cursor + flat.len() - 1) % flat.len();
            }
            if !self.running && enter {
                picked = Some(flat[self.model_cursor].clone());
            }
        }
        let current = self.settings.model.clone();
        let context_window = self.settings.context_window;
        let running = self.running;
        let mut filter = std::mem::take(&mut self.model_filter);
        let mut hover = None;
        let mut clicked = None;

        // Grouped by provider, so a long catalog still reads as a short list.
        let groups: Vec<(String, Vec<(String, String)>)> = self
            .catalog
            .as_ref()
            .map(|cat| {
                cat.providers
                    .iter()
                    .filter(|(pid, _)| available.contains(*pid))
                    .filter_map(|(pid, p)| {
                        let rows: Vec<(String, String)> = p
                            .models
                            .iter()
                            .filter(|m| {
                                query.is_empty()
                                    || format!("{pid}/{}", m.id).to_lowercase().contains(&query)
                            })
                            .map(|m| (m.id.clone(), context_label(context_window, m)))
                            .collect();
                        (!rows.is_empty()).then(|| (pid.clone(), rows))
                    })
                    .collect()
            })
            .unwrap_or_default();

        sheet(ctx, "model-picker", "Choose a model")
            .width(520.0)
            .scrim(Scrim::Soft)
            .lead("Type to search · ↑ / ↓ choose · Enter selects")
            .top(|ui| {
                search(ui, "Search models or providers…", &mut filter, "model-search");
                if flat.is_empty() {
                    ui.add_space(8.0);
                    hint(ui, "No usable models match. Use +add to add an API or sign in.");
                } else if running {
                    ui.add_space(8.0);
                    hint(ui, "Model changes are available after the current run.");
                }
            })
            .body(|ui| {
                ui.add_enabled_ui(!running, |ui| {
                    for (pid, rows) in &groups {
                        ui.add_space(8.0);
                        ui.label(egui::RichText::new(pid).size(12.0).color(theme::TEAL));
                        ui.add_space(2.0);
                        for (id, meta) in rows {
                            let full = format!("{pid}/{id}");
                            let selected =
                                flat.get(self.model_cursor).is_some_and(|(p, i)| {
                                    p == pid && i == id
                                });
                            let active = full == current;
                            let response = choice_row(ui, id, meta, selected, active);
                            if selected && (down || up) {
                                response.scroll_to_me(Some(egui::Align::Center));
                            }
                            if response.hovered()
                                && ui.input(|i| {
                                    i.events.iter().any(|e| {
                                        matches!(e, egui::Event::PointerMoved(_))
                                    })
                                })
                            {
                                hover = flat.iter().position(|(p, i)| p == pid && i == id);
                            }
                            if response.clicked() {
                                clicked = Some((pid.clone(), id.clone()));
                            }
                        }
                    }
                });
            })
            .bottom(|ui| {
                if running {
                    footnote(ui, "Stop the run to switch models");
                } else {
                    footnote(ui, "Esc closes");
                }
            })
            .show(&mut open);

        self.model_filter = filter;
        if let Some(index) = hover {
            self.model_cursor = index;
        }
        self.show_models = open;
        if let Some((pid, id)) = clicked.or(picked) {
            self.select_model(&pid, &id);
            self.show_models = false;
        }
    }

    /// The thinking-effort sheet: the level as a large sage numeral, the
    /// gradient bar, and the two ends of the scale named underneath. Applies on
    /// release, the way the arrow keys do.
    fn thinking_picker_window(&mut self, ctx: &egui::Context) {
        if !self.show_thinking_picker {
            return;
        }
        let levels = self.effort_levels();
        let max = levels.len() - 1;
        self.thinking_slider = self.thinking_slider.min(max);
        let mut open = self.show_thinking_picker;
        let mut apply = None;
        let (left, right) = ctx.input_mut(|i| {
            (
                i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowLeft),
                i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowRight),
            )
        });
        if max > 0 {
            let mut moved = false;
            if left && self.thinking_slider > 0 {
                self.thinking_slider -= 1;
                moved = true;
            }
            if right && self.thinking_slider < max {
                self.thinking_slider += 1;
                moved = true;
            }
            if moved {
                apply = Some(levels[self.thinking_slider].clone());
            }
        }
        let current = self.thinking_slider;
        let numeric = max > 0;
        let title = levels[current].clone();
        let low = levels[0].clone();
        let high = levels[max].clone();
        let mut picked = None;

        sheet(ctx, "thinking-picker", "Thinking effort")
            .width(440.0)
            .scrim(Scrim::Soft)
            .lead("Give Feather a little more time to think.")
            .top(|ui| {
                tag(ui, &format!("{} of {}", current + 1, levels.len()), TEAL);
            })
            .body(|ui| {
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new(&title)
                        .size(26.0)
                        .strong()
                        .color(theme::TEAL_BRIGHT),
                );
                ui.add_space(12.0);
                picked = effort_bar(ui, "thinking-effort", levels.len(), current, numeric);
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(&low).size(12.0).color(MUTED));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(egui::RichText::new(&high).size(12.0).color(MUTED));
                    });
                });
                ui.add_space(10.0);
                if numeric {
                    hint(ui, "Manual effort presets · configure 'Reasoning not supported' in +add to disable.");
                } else {
                    hint(ui, "This model has no adjustable thinking effort.");
                }
            })
            .bottom(|ui| {
                footnote(
                    ui,
                    if numeric {
                        "← / → adjusts · release to apply"
                    } else {
                        "Esc closes"
                    },
                );
            })
            .show(&mut open);

        self.show_thinking_picker = open;
        if let Some(level) = picked.map(|index| levels[index].clone()).or(apply) {
            self.thinking_applied = self.thinking_slider;
            self.set_thinking(&level);
        }
    }

    fn settings_window(&mut self, ctx: &egui::Context) {
        if !self.show_settings {
            return;
        }
        let mut open = self.show_settings;
        let mut reload = false;
        let mut toggle_resume = None;
        let overrides = config::feather_home().join("config.redb");
        let rows: Vec<(String, String, bool)> = self
            .kb
            .table()
            .iter()
            .map(|(id, combos)| {
                let spec = if combos.is_empty() {
                    "(disabled)".to_string()
                } else {
                    combos
                        .iter()
                        .map(|c| c.to_spec())
                        .collect::<Vec<_>>()
                        .join(", ")
                };
                let overridden = self.kb.is_overridden(id);
                (id.clone(), spec, overridden)
            })
            .collect();
        let resume = self.settings.auto_resume_latest;

        sheet(ctx, "keybindings", "Keybindings")
            .width(640.0)
            .scrim(Scrim::Deep)
            .lead("Every action can be remapped in config.redb; an empty list disables it.")
            .top(|ui| {
                toggle_resume = Some(check_row(
                    ui,
                    resume,
                    "Continue latest workspace session on startup",
                ));
                ui.add_space(8.0);
                hint(
                    ui,
                    &format!("Overrides: {}", overrides.display()),
                );
                reload = ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::F5));
                if reload {
                    hint(ui, "Reloading from config.redb…");
                }
            })
            .body(|ui| {
                for (id, spec, overridden) in &rows {
                    let response = list_row(ui, false, |ui| {
                        ui.horizontal(|ui| {
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(id).size(12.0).color(TEXT),
                                )
                                .truncate(),
                            );
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if *overridden {
                                        tag(ui, "override", TEAL);
                                        ui.add_space(6.0);
                                    }
                                    ui.label(
                                        egui::RichText::new(spec).size(11.5).color(if *overridden {
                                            TEAL
                                        } else {
                                            MUTED
                                        }),
                                    );
                                },
                            );
                        });
                    });
                    let _ = response;
                }
            })
            .bottom(|ui| {
                footnote(
                    ui,
                    "F1 shortcuts · Ctrl+L models · Ctrl+Shift+T thinking · Ctrl+Shift+N new",
                );
            })
            .show(&mut open);

        self.show_settings = open;
        if let Some(value) = toggle_resume {
            if value != resume {
                self.settings.auto_resume_latest = value;
                if let Err(e) = feather::preferences::save_settings_patch(
                    &config::feather_home().join("config.redb"),
                    &serde_json::json!({"autoResumeLatest": value}),
                ) {
                    self.rows.push(ChatRow::new(RowKind::Error, e.to_string()));
                }
            }
        }
        if reload {
            self.kb = keybindings::Keybindings::load();
        }
    }

    fn spawn_login(&mut self, p: &'static oauth::OAuthProvider) {
        if self.preview {
            return;
        }
        if !self.auth_pending.insert(p.id) {
            return;
        }
        let tx = self.ev_tx.clone();
        let prompt_tx = tx.clone();
        let ctx = self.egui_ctx.clone();
        let provider = p.id;
        std::thread::spawn(move || {
            let notify = |pr: oauth::LoginPrompt| {
                let prompt = if pr.user_code.is_empty() {
                    format!("{provider}: continue sign-in at {}", pr.url)
                } else {
                    format!("{provider}: enter code {} at {}", pr.user_code, pr.url)
                };
                let _ = prompt_tx.send(UiEvent::Agent(AgentEvent::Notice(prompt)));
                ctx.request_repaint();
            };
            let result = oauth::run_login_with_notify(p, &notify)
                .and_then(|o| oauth::AuthStore::update(provider, Some(o.account)));
            let _ = tx.send(UiEvent::Auth {
                provider,
                action: "signed in",
                result,
            });
            ctx.request_repaint();
        });
    }

    fn spawn_auth_update(&mut self, provider: &'static str, account: Option<oauth::OAuthAccount>) {
        if self.preview {
            return;
        }
        if !self.auth_pending.insert(provider) {
            return;
        }
        let action = if account.is_some() {
            "API key saved"
        } else {
            "signed out (environment keys are unchanged)"
        };
        let tx = self.ev_tx.clone();
        let ctx = self.egui_ctx.clone();
        std::thread::spawn(move || {
            let result = oauth::AuthStore::update(provider, account);
            let _ = tx.send(UiEvent::Auth {
                provider,
                action,
                result,
            });
            ctx.request_repaint();
        });
    }

    /// `AuthStore` memoized on the config database's (mtime, len).
    ///
    /// The login window renders every frame, so reading the credential store
    /// from redb per frame is pure waste. The stamp keeps the behavior exact:
    /// any write — this process or another one — changes the file and forces a
    /// reload.
    fn cached_auth_store(home: &std::path::Path) -> Result<oauth::AuthStore, String> {
        fn stamp(path: &std::path::Path) -> Option<(u64, u64)> {
            let meta = std::fs::metadata(path).ok()?;
            let mtime = meta
                .modified()
                .ok()?
                .duration_since(std::time::UNIX_EPOCH)
                .ok()?
                .as_millis() as u64;
            Some((mtime, meta.len()))
        }
        type Cache = std::sync::Mutex<Option<(Option<(u64, u64)>, oauth::AuthStore)>>;
        static CACHE: std::sync::OnceLock<Cache> = std::sync::OnceLock::new();
        let path = home.join("config.redb");
        let current = stamp(&path);
        let cache = CACHE.get_or_init(|| std::sync::Mutex::new(None));
        if let Ok(guard) = cache.lock() {
            if let Some((cached, store)) = guard.as_ref() {
                if *cached == current {
                    return Ok(store.clone());
                }
            }
        }
        let store = oauth::AuthStore::load_from(&path)?;
        if let Ok(mut guard) = cache.lock() {
            *guard = Some((current, store.clone()));
        }
        Ok(store)
    }

    /// The add-API / sign-in sheet. One card, two modes: a compatible-endpoint
    /// form or a list of built-in providers. The footer keeps Save/Clear and the
    /// type-a-limit field out of the scrolling body, so an error never hides a
    /// control the user needs.
    fn login_window(&mut self, ctx: &egui::Context) {
        if !self.show_login {
            return;
        }
        let auth_error = match Self::cached_auth_store(&self.home) {
            Ok(store) => {
                self.auth = store;
                None
            }
            Err(e) => Some(e),
        };
        let mut open = self.show_login;
        let wide = ctx.screen_rect().width() >= 900.0;
        let typing = ctx.wants_keyboard_input();
        let (up, down, enter, delete, ctrl_o) = ctx.input_mut(|i| {
            (
                i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp),
                i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown),
                i.consume_key(egui::Modifiers::NONE, egui::Key::Enter),
                i.consume_key(egui::Modifiers::NONE, egui::Key::Delete),
                i.consume_key(egui::Modifiers::CTRL, egui::Key::O),
            )
        });
        let compatible = self.model_manager.enabled;
        if !compatible && !typing {
            if down {
                self.auth_selected = (self.auth_selected + 1) % oauth::PROVIDERS.len();
            }
            if up {
                self.auth_selected =
                    (self.auth_selected + oauth::PROVIDERS.len() - 1) % oauth::PROVIDERS.len();
            }
        }
        let selected = oauth::PROVIDERS[self.auth_selected];
        let pending = self.auth_pending.contains(selected.id);
        // The sheet keeps its region closures in one struct, so they all capture
        // at once: intents and the key draft are shared through cells, then
        // applied once the card has been drawn.
        let sign_in: std::cell::Cell<Option<&'static oauth::OAuthProvider>> =
            std::cell::Cell::new(None);
        let sign_out: std::cell::Cell<Option<&'static str>> = std::cell::Cell::new(None);
        let manage: std::cell::Cell<Option<model_manager::Action>> = std::cell::Cell::new(None);
        let save_key: std::cell::Cell<Option<String>> = std::cell::Cell::new(None);
        let draft_init = self.api_key_drafts.remove(selected.id).unwrap_or_default();
        let draft = std::cell::RefCell::new(draft_init.clone());
        let mut mode = usize::from(!compatible);
        let rows: Vec<(String, String, bool, bool)> = oauth::PROVIDERS
            .iter()
            .map(|p| {
                let logged = self.auth.valid(p.id);
                let waiting = self.auth_pending.contains(p.id);
                let status = self
                    .auth
                    .get(p.id)
                    .map(|a| {
                        if a.kind == "api_key" {
                            "API key saved".into()
                        } else if logged {
                            format_expires(a.expires)
                        } else {
                            "renew on next request".into()
                        }
                    })
                    .unwrap_or_else(|| "not signed in".into());
                (p.id.to_string(), status, logged, waiting)
            })
            .collect();
        let highlight = self.auth_selected;
        let key_id = egui::Id::new("login-api-key");
        // The card's three regions are separate closures, so the mutable state
        // they share moves out of `self` for the duration of the draw: a shared
        // handle for the form, and a local string for the key draft.
        let form = std::rc::Rc::new(std::cell::RefCell::new(std::mem::take(
            &mut self.model_manager,
        )));
        let signed_in = self.auth.get(selected.id).is_some();
        let hover: std::cell::Cell<Option<usize>> = std::cell::Cell::new(None);
        let catalog = self.catalog.clone();
        let home = self.home.clone();
        let active_model = self.settings.model.clone();
        let running = self.running;
        let global = (
            self.settings.context_window,
            self.settings.compaction.reserve_tokens,
            self.settings.compaction.keep_recent_tokens,
        );
        let provider_id = selected.id;

        sheet(ctx, "add-api", "Add API · manage models")
            .width(if wide { 540.0 } else { 500.0 })
            .scrim(Scrim::Deep)
            // A sign-in in flight can't be dismissed: closing the card would
            // hide the only place its progress and errors are reported.
            .closable(!pending)
            .lead("Connect a provider, then choose the models you want to use.")
            .top(|ui| {
                if let Some(error) = &auth_error {
                    error_line(ui, error);
                    ui.add_space(8.0);
                }
                segmented(
                    ui,
                    "add-api-mode",
                    &["Compatible API / models", "OAuth / built-in providers"],
                    &mut mode,
                );
                ui.add_space(8.0);
                if let Some(action) = form
                    .borrow_mut()
                    .show_context_settings(ui, catalog.as_ref(), &active_model, global, running)
                {
                    manage.set(Some(action));
                }
            })
            .body(|ui| {
                if compatible {
                    if let Some(action) = form.borrow_mut().show(ui, &home, &active_model, running) {
                        manage.set(Some(action));
                    }
                    return;
                }
                for (index, (pid, status, logged, waiting)) in rows.iter().enumerate() {
                    let response = list_row(ui, index == highlight, |ui| {
                        ui.horizontal(|ui| {
                            let (dot, _) =
                                ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
                            ui.painter().circle_filled(
                                dot.center(),
                                3.0,
                                if *logged { TEAL } else { MUTED },
                            );
                            ui.add(
                                egui::Label::new(egui::RichText::new(pid).size(13.0).color(TEXT))
                                    .truncate(),
                            );
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if *waiting {
                                        ui.spinner();
                                    }
                                    ui.label(
                                        egui::RichText::new(status).size(11.5).color(MUTED),
                                    );
                                },
                            );
                        });
                    });
                    if response.hovered()
                        && ui.input(|i| {
                            i.events
                                .iter()
                                .any(|e| matches!(e, egui::Event::PointerMoved(_)))
                        })
                    {
                        hover.set(Some(index));
                    }
                    if response.clicked() {
                        hover.set(Some(index));
                    }
                }
                if selected.kind == oauth::Flow::ApiKey {
                    ui.add_space(8.0);
                    hint(
                        ui,
                        &format!(
                            "{} · paste key, then Enter to save · Ctrl+O opens the key portal",
                            provider_id
                        ),
                    );
                    ui.add_space(4.0);
                    ui.add_enabled_ui(!pending && auth_error.is_none(), |ui| {
                        field(ui, "login-api-key", &mut draft.borrow_mut(), "Paste API key…");
                    });
                    if ui.memory(|m| m.has_focus(key_id)) && enter {
                        let typed = draft.borrow().trim().to_owned();
                        if !typed.is_empty() {
                            save_key.set(Some(typed));
                        }
                    }
                } else {
                    ui.add_space(8.0);
                    hint(ui, "OAuth opens your browser. Credentials renew automatically.");
                }
            })
            .bottom(|ui| {
                if compatible {
                    if let Some(action) = form.borrow_mut().show_footer(ui, running) {
                        manage.set(Some(action));
                    }
                    footnote(ui, "Model changes apply to the next request");
                } else if selected.kind == oauth::Flow::ApiKey {
                    if button(ui, "Save key", ButtonKind::Primary).clicked() {
                        let typed = draft.borrow().trim().to_owned();
                        if !typed.is_empty() {
                            save_key.set(Some(typed));
                        }
                    }
                    footnote(ui, "↑ / ↓ provider · Esc closes");
                } else {
                    if button(ui, "Sign in", ButtonKind::Primary).clicked()
                        || (!typing && enter && !pending && auth_error.is_none())
                    {
                        sign_in.set(Some(selected));
                    }
                    if signed_in && button(ui, "Sign out", ButtonKind::Danger).clicked() {
                        sign_out.set(Some(provider_id));
                    }
                    if !typing && delete && signed_in {
                        sign_out.set(Some(provider_id));
                    }
                    footnote(ui, "↑ / ↓ provider · Enter signs in · Del signs out");
                }
            })
            .show(&mut open);

        // Hand the form back, keeping whatever the user typed this frame.
        let mut form = std::rc::Rc::try_unwrap(form)
            .map(std::cell::RefCell::into_inner)
            .unwrap_or_else(|shared| shared.borrow().clone());
        form.enabled = mode == 0;
        self.model_manager = form;
        let typed = draft.into_inner();
        if typed != draft_init {
            self.api_key_drafts.insert(provider_id.to_owned(), typed);
        }
        if let Some(index) = hover.into_inner() {
            self.auth_selected = index;
        }
        if let Some(provider) = sign_in.into_inner() {
            self.spawn_login(provider);
        }
        if let Some(id) = sign_out.into_inner() {
            self.spawn_auth_update(id, None);
        }
        if ctrl_o && !compatible && selected.kind == oauth::Flow::ApiKey {
            if let Err(error) = oauth::open_browser(selected.authorize_url) {
                self.rows.push(ChatRow::new(RowKind::Error, error));
            }
        }
        if let Some(key) = save_key.into_inner() {
            self.spawn_auth_update(provider_id, Some(oauth::OAuthAccount::api_key(key)));
            self.api_key_drafts.remove(provider_id);
        }
        if let Some(action) = manage.into_inner() {
            self.manage_model(action);
        }
        // The sheet owns the close state for this frame.
        self.show_login = open;
    }
}

/// The window a model really gets: the global context cap, clamped by the
/// model's own limit. Shown on the picker row so the number matches the agent.
fn context_label(global: u64, model: &models::ModelEntry) -> String {
    let effective = models::effective_context_window(global, Some(model));
    match model.context_window {
        Some(cap) if cap > effective => {
            format!("{}k of {}k context", effective / 1000, cap / 1000)
        }
        _ => format!("{}k context", effective / 1000),
    }
}
fn format_expires(millis: u64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let hours = millis.saturating_sub(now) / 3_600_000;
    if hours > 24 {
        format!("~{}d left", hours / 24)
    } else {
        format!("~{}h left", hours)
    }
}

// ---------------------------------------------------------------------------
// background plumbing
// ---------------------------------------------------------------------------

/// One producer owns prompt events and completion so RunDone cannot overtake
/// final assistant messages. RunEnd is only an inner-run boundary (follow-ups
/// may still be queued), not permission to submit a second competing prompt.
async fn drive_prompt(
    agent: Arc<tokio::sync::Mutex<Agent>>,
    text: String,
    tx: Sender<UiEvent>,
    ctx: egui::Context,
) {
    let mut guard = agent.lock().await;
    let mut events = guard.subscribe();
    let mut prompt =
        Box::pin(guard.prompt_message_prepared(feather_core::message::Message::user_text(text)));
    let result = loop {
        tokio::select! {
            result=&mut prompt=>break result.map_err(|e|e.to_string()),
            event=events.recv()=>match event {
                Ok(event)=>{if tx.send(UiEvent::Agent(event)).is_err(){return;}ctx.request_repaint_after(std::time::Duration::from_millis(33));},
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_))=>{},
                Err(tokio::sync::broadcast::error::RecvError::Closed)=>break Err("agent event stream closed".into()),
            }
        }
    };
    drop(prompt);
    loop {
        match events.try_recv() {
            Ok(event) => {
                if tx.send(UiEvent::Agent(event)).is_err() {
                    return;
                }
            }
            Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => continue,
            Err(_) => break,
        }
    }
    drop(guard);
    let _ = tx.send(UiEvent::RunDone(result));
    ctx.request_repaint();
}

#[cfg(test)]
fn ellipsize(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.into();
    }
    let mut result: String = text.chars().take(max_chars.saturating_sub(1)).collect();
    result.push('…');
    result
}
fn truncate_utf8(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_string();
    }
    let mut end = limit;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n… (truncated)", &text[..end])
}

fn tool_summary(args: &serde_json::Value) -> String {
    for key in ["path", "command", "pattern", "task", "url"] {
        if let Some(value) = args.get(key).and_then(|v| v.as_str()) {
            return truncate_utf8(value.lines().next().unwrap_or(value), 180);
        }
    }
    feather_core::tool::tool_args_summary(args, 180)
}

/// Render an original audit page (or an in-memory test fixture), NOT the
/// compacted LLM context. Tool results may straddle a page boundary; history
/// merges the orphan with its original call when the older page is loaded.
fn rows_from_messages(messages: &[feather_core::message::Message]) -> Vec<ChatRow> {
    let mut rows: Vec<ChatRow> = Vec::new();
    for message in messages {
        match message.role {
            Role::User => {
                let text = message.text();
                if text.starts_with("<feather-runtime-diagnostic>") {
                    rows.push(ChatRow::new(RowKind::Notice, text));
                    continue;
                }
                if !text.is_empty() {
                    rows.push(ChatRow::new(RowKind::User, text));
                }
            }
            Role::Assistant => {
                let text = message.text();
                let thinking = message.thinking();
                if !text.is_empty() || !thinking.is_empty() {
                    let mut row = ChatRow::new(RowKind::Assistant, text);
                    row.thinking = thinking;
                    rows.push(row);
                }
                for call in message.tool_calls() {
                    let mut row = ChatRow::new(RowKind::Tool, &call.name);
                    row.tool_id = Some(call.id.clone());
                    row.tool_args = tool_summary(&call.arguments);
                    rows.push(row);
                }
            }
            Role::ToolResult => {
                let output = truncate_utf8(&message.text(), 20_000);
                let id = message.tool_call_id.clone();
                if let Some(row) = rows
                    .iter_mut()
                    .rev()
                    .find(|row| row.tool_id == id && row.tool_output.is_none())
                {
                    row.tool_output = Some(output);
                    row.tool_error = message.is_error;
                } else {
                    let mut row =
                        ChatRow::new(RowKind::Tool, message.tool_name.clone().unwrap_or_default());
                    row.tool_id = id;
                    row.tool_output = Some(output);
                    row.tool_error = message.is_error;
                    rows.push(row);
                }
            }
            Role::System => {}
        }
    }
    rows
}

/// Compact path for the top bar: keep the first and last two segments.
fn short_path(p: &std::path::Path) -> String {
    let parts: Vec<String> = p
        .components()
        .map(|c| c.as_os_str().to_string_lossy().to_string())
        .collect();
    if parts.len() <= 3 {
        return parts.join("\\");
    }
    format!(
        "{}\\...\\{}\\{}",
        parts[0],
        parts[parts.len() - 2],
        parts[parts.len() - 1]
    )
}

// ---------------------------------------------------------------------------
// eframe entry
// ---------------------------------------------------------------------------

impl eframe::App for FeatherApp {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        // Adjacent antialiased panel edges can leave a one-pixel coverage gap.
        // eframe's default is translucent dark gray, which exposed a GPU-only
        // line at the composer boundary. Use the same opaque canvas as CPU.
        PANEL.to_normalized_gamma_f32()
    }
    fn raw_input_hook(&mut self, ctx: &egui::Context, input: &mut egui::RawInput) {
        self.frame_pacer.before_frame();
        graphics::bound_font_texture(input);
        if self.preview
            && std::env::var("FEATHER_UI_PREVIEW_HOVER").ok().as_deref() == Some("close")
        {
            let screen = input.screen_rect.unwrap_or_else(|| ctx.screen_rect());
            input.events.push(egui::Event::PointerMoved(egui::pos2(
                screen.right() - 30.0,
                30.0,
            )));
        }
    }
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.update_ui(ctx);
    }
}

impl FeatherApp {
    fn update_ui(&mut self, ctx: &egui::Context) {
        self.preview_timing.tick(ctx);
        if ctx.input(|i| i.viewport().close_requested()) && !self.exit_ready {
            self.request_close();
        }
        // stream events
        self.drain_events();
        if self.history.poll(&mut self.rows) {
            cards::release_history_layouts(ctx);
        }
        self.proxies.poll();
        if self.exit.is_some() {
            self.shutdown_ui(ctx);
        } else {
            self.frame_ui(ctx);
        }
        self.poll_exit(ctx);
        if self.preview {
            if let Ok(path) = std::env::var("FEATHER_UI_CAPTURE") {
                self.capture_frames += 1;
                if self.capture_frames == 8
                    && std::env::var("FEATHER_UI_PREVIEW_SIZE").ok().as_deref() == Some("maximized")
                {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(true));
                }
                if self.capture_frames == 24 {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot);
                }
                let image = ctx.input(|i| {
                    i.events.iter().find_map(|e| {
                        if let egui::Event::Screenshot { image, .. } = e {
                            Some(image.clone())
                        } else {
                            None
                        }
                    })
                });
                if let Some(image) = image {
                    let pixels: Vec<u8> = image.pixels.iter().flat_map(|c| c.to_array()).collect();
                    if let Some(parent) = std::path::Path::new(&path).parent() {
                        let _ = std::fs::create_dir_all(parent);
                    }
                    if let Err(e) = image::save_buffer(
                        &path,
                        &pixels,
                        image.size[0] as u32,
                        image.size[1] as u32,
                        image::ColorType::Rgba8,
                    ) {
                        eprintln!("preview capture: {e}");
                    }
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                } else {
                    ctx.request_repaint_after(std::time::Duration::from_millis(70));
                }
            }
        }
    }
}

impl FeatherApp {
    /// Everything eframe asks us to paint for one frame: global shortcuts, the
    /// panel layout and, on top of it, the modal windows. Split out of
    /// `eframe::App::update` (which additionally pumps agent events) so the
    /// layout can be driven headlessly by tests.
    fn frame_ui(&mut self, ctx: &egui::Context) {
        // Winit/eframe can synchronously render the transient 199x34 Windows
        // minimized client rectangle. Never lay out whole panels in that frame;
        // update_ui still drains events and services close/restart outside here.
        if ctx.input(|i| i.viewport().minimized == Some(true))
            || ctx.screen_rect().height() < 120.0
            || ctx.screen_rect().width() < 300.0
        {
            return;
        }
        // (agent events were pumped by `update` before this call)
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::F11)) {
            let full = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(!full));
        }
        self.rewind_turns.sync(&self.input, ctx);
        self.skill_choices
            .sync(&self.input, &self.root, &self.home, ctx);
        self.skill_choices.poll();
        self.rewind_turns.poll();
        let suggestions = self.cached_suggestions();
        let modal = self.show_models
            || self.show_login
            || self.show_settings
            || self.show_thinking_picker
            || self.renderer_picker.is_some()
            || self.request_picker.is_some()
            || self.picker.is_some();
        let mut cursor = self.command_cursor;
        let mut selected = None;
        if !suggestions.is_empty() && !modal {
            cursor = cursor.min(suggestions.len() - 1);
            if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown)) {
                cursor = (cursor + 1) % suggestions.len();
            }
            if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp)) {
                cursor = (cursor + suggestions.len() - 1) % suggestions.len();
            }
            if ctx.input_mut(|i| {
                i.consume_key(egui::Modifiers::NONE, egui::Key::Tab)
                    || i.consume_key(egui::Modifiers::NONE, egui::Key::Enter)
            }) {
                selected = Some(suggestions[cursor].clone());
            }
        }
        drop(suggestions);
        self.command_cursor = cursor;
        if let Some(choice) = selected {
            self.accept_suggestion(choice, ctx);
        }
        // Native dialogs always offer Esc, regardless of terminal registry
        // precedence or a disabled interrupt binding. Consume it before widgets.
        if (self.show_models
            || self.show_thinking_picker
            || self.renderer_picker.is_some()
            || self.request_picker.is_some()
            || self.show_login
            || self.show_settings
            || self.picker.is_some())
            && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
        {
            self.handle_action("app.interrupt", ctx);
        }

        // global keybindings (pi parity): resolve without cloning the event list
        let actions: Vec<(&'static str, egui::Modifiers, egui::Key)> = ctx.input(|i| {
            i.events
                .iter()
                .filter_map(|ev| match ev {
                    egui::Event::Key {
                        key,
                        modifiers,
                        pressed: true,
                        repeat: false,
                        ..
                    } => keybindings::egui_event_to_action(&self.kb, *modifiers, *key)
                        .map(|a| (a, *modifiers, *key)),
                    _ => None,
                })
                .collect()
        });
        for (action, modifiers, key) in actions {
            let modal = self.show_login
                || self.show_models
                || self.show_settings
                || self.show_thinking_picker
                || self.renderer_picker.is_some()
                || self.request_picker.is_some()
                || self.picker.is_some();
            let composer_focused = ctx.memory(|m| m.has_focus(egui::Id::new("feather-composer")));
            if action == "tui.input.submit" && (!composer_focused || modal) {
                continue;
            }
            if modal
                && !matches!(
                    action,
                    "app.interrupt"
                        | "tui.select.cancel"
                        | "tui.altScreen.searchClose"
                        | "app.shortcuts"
                        | "app.model.select"
                        | "app.thinking.select"
                )
            {
                continue;
            }
            if action.starts_with("app.") || action == "tui.input.submit" {
                ctx.input_mut(|i| i.consume_key(modifiers, key));
            }
            self.handle_action(action, ctx);
        }

        if self.running {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        egui::TopBottomPanel::top("feather_top")
            .show_separator_line(false)
            .resizable(false)
            .frame(egui::Frame::none().fill(PANEL).stroke(egui::Stroke::NONE))
            .show(ctx, |ui| self.top_bar(ui));
        // Header X can begin shutdown within this very frame; skip panels that
        // may synchronously read config immediately after that click.
        if self.exit.is_some() {
            return;
        }
        self.tree_panel(ctx);
        egui::TopBottomPanel::bottom("feather_input")
            .show_separator_line(false)
            .resizable(false)
            .frame(
                egui::Frame::none()
                    .fill(PANEL)
                    .stroke(egui::Stroke::NONE)
                    .inner_margin(egui::Margin::symmetric(18.0, 8.0)),
            )
            .show(ctx, |ui| self.input_bar(ui));

        if self.exit.is_some() {
            return;
        }
        egui::CentralPanel::default()
            .frame(
                egui::Frame::none()
                    .fill(PANEL)
                    .stroke(egui::Stroke::NONE)
                    .inner_margin(egui::Margin::symmetric(14.0, 8.0)),
            )
            .show(ctx, |ui| self.chat_panel(ui));

        // Each sheet paints its own veil, so there is no page-wide scrim here.
        self.resume_window(ctx);
        self.requests_window(ctx);
        self.renderer_window(ctx);
        self.models_window(ctx);
        self.thinking_picker_window(ctx);
        self.settings_window(ctx);
        self.login_window(ctx);
    }
}

fn main() -> anyhow::Result<()> {
    // A renderer child never waits on itself; the parent is only a prior GUI
    // instance handed off by this process.
    lifecycle::wait_for_parent()?;
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--install-context-menu") {
        let exe = std::env::current_exe()?;
        context_menu::install(&exe).map_err(anyhow::Error::msg)?;
        println!(
            "registered Explorer right-click menu: {}",
            context_menu::MENU_LABEL
        );
        return Ok(());
    }
    if args.iter().any(|a| a == "--uninstall-context-menu") {
        context_menu::uninstall().map_err(anyhow::Error::msg)?;
        println!("right-click menu removed");
        return Ok(());
    }
    // An update can leave the Explorer shortcut pointing at a binary that no
    // longer exists (a moved install directory, or a throwaway verification
    // copy that registered itself). Heal it from the running executable, but
    // never during a preview/test run and never over a live registration.
    if std::env::var("FEATHER_UI_PREVIEW").ok().as_deref() != Some("1") {
        if let Ok(exe) = std::env::current_exe() {
            let _ = context_menu::repair_if_stale(&exe);
        }
    }
    // positional path = project root (Explorer passes the right-clicked dir)
    let root = args
        .first()
        .filter(|a| !a.starts_with('-'))
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .ok_or_else(|| anyhow::anyhow!("no working directory"))?;

    let icon = load_icon();
    let size = if std::env::var("FEATHER_UI_PREVIEW_SIZE").ok().as_deref() == Some("compact") {
        [760.0, 560.0]
    } else {
        [1160.0, 820.0]
    };
    let mut options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size(size)
            .with_maximized(
                std::env::var("FEATHER_UI_PREVIEW").ok().as_deref() == Some("1")
                    && std::env::var("FEATHER_UI_PREVIEW_SIZE").ok().as_deref()
                        == Some("maximized"),
            )
            .with_min_inner_size([760.0, 520.0])
            .with_title("Feather")
            .with_decorations(false)
            .with_icon(icon),
        ..Default::default()
    };
    let preview = std::env::var("FEATHER_UI_PREVIEW").ok().as_deref() == Some("1");
    let renderer = renderer_choice::selected(&config::feather_home(), preview);
    let requested_native = std::env::var("FEATHER_GUI_RENDERER").ok();
    if !matches!(requested_native.as_deref(), Some("dx12" | "vulkan")) {
        std::env::set_var("FEATHER_GUI_RENDERER", renderer.env());
    }

    if renderer == renderer_choice::Backend::Cpu {
        software_window::run(root, options.viewport)?;
        lifecycle::launch_pending()?;
        return Ok(());
    }

    let gpu_initialized = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let initialized = gpu_initialized.clone();
    let app_root = root.clone();
    graphics::apply(&mut options);
    if preview
        && std::env::var("FEATHER_UI_PREVIEW_GPU_UNAVAILABLE")
            .ok()
            .as_deref()
            == Some("1")
    {
        options.wgpu_options.supported_backends = eframe::wgpu::Backends::empty();
    }
    let native_result = eframe::run_native(
        "Feather",
        options,
        Box::new(move |cc| {
            initialized.store(true, std::sync::atomic::Ordering::Relaxed);
            match FeatherApp::new(cc, app_root) {
                Ok(app) => Ok(Box::new(app) as Box<dyn eframe::App>),
                Err(e) => Err(Box::new(std::io::Error::other(e.to_string()))),
            }
        }),
    )
    .map_err(|e| anyhow::anyhow!("eframe: {e}"));
    if let Err(error) = native_result {
        if gpu_initialized.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(error);
        }
        // Startup failed before touching the session/handoff. Open the CPU host
        // instead of leaving saved GPU users unable to launch. No WGL fallback.
        eprintln!("Native GPU initialization unavailable; starting CPU renderer ({error}).");
        std::env::set_var("FEATHER_GUI_RENDERER", "cpu");
        gpu_fallback::launch_cpu(&root)?;
        return Ok(());
    }
    lifecycle::launch_pending()?;
    Ok(())
}

fn load_icon() -> egui::IconData {
    let img = image::load_from_memory(LOGO_PNG).expect("embedded logo");
    let rgba = img.to_rgba8();
    egui::IconData {
        width: rgba.width(),
        height: rgba.height(),
        rgba: rgba.into_raw(),
    }
}
