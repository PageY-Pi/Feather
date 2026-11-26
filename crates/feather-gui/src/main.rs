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
mod clipboard_image;
mod commands;
mod context_menu;
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
mod ui;
mod window_chrome;
mod workspace;
// Re-exported at the crate root: `#[cfg(test)]` modules do `use super::*` and
// expect the palette names to resolve, exactly as they did before `ui::` split.
use theme::{apply_theme, install_fonts, PANEL};
#[cfg(test)]
use theme::{INK, MUTED, PANEL_2, RED, SOFT_ACCENT, TEAL, TEXT};

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

/// An image shown inside a transcript row.
///
/// The texture is created lazily on first paint and then cached by `ui_key`, so a
/// re-render (scroll, hover, streaming) does not re-decode the PNG. `None` means
/// "not uploaded yet"; once decoded the handle lives for the row's lifetime.
#[derive(Clone)]
struct RowImage {
    media_type: String,
    data: String,
    /// Uploaded texture, filled in on first paint.
    texture: Option<egui::TextureHandle>,
    /// Set once decoding has been attempted and failed.
    ///
    /// Without this, a payload that cannot be decoded is retried on every frame,
    /// and `image.decode` measures ~1.1 ms for a 1 MP attachment — so one bad
    /// image would hold the whole UI at well under 60 fps forever. The row shows
    /// a placeholder instead, at zero per-frame cost.
    failed: bool,
}

impl std::fmt::Debug for RowImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The base64 payload is huge and would swamp any Debug output.
        f.debug_struct("RowImage")
            .field("media_type", &self.media_type)
            .field("bytes", &self.data.len())
            .field("uploaded", &self.texture.is_some())
            .finish()
    }
}
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
    /// One-line subject shown in the card header (file path, or the model's
    /// stated intent). Avoids dumping opaque anchors at the user.
    tool_args: String,
    /// The raw command or arguments, revealed when the card is expanded. Empty
    /// when the header already shows the only thing worth reading.
    tool_detail: String,
    started_at: Option<f64>,
    elapsed: Option<f64>,
    /// Base64 images attached to this row, in send order.
    ///
    /// Kept on the row rather than looked up from the session so a reloaded
    /// transcript and a live one render identically: `rows_from_messages` fills
    /// this from the message content blocks, and `submit` fills it from the same
    /// staged attachments it sends.
    images: Vec<RowImage>,
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
            tool_detail: String::new(),
            started_at: None,
            elapsed: None,
            images: Vec::new(),
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
    /// The broadcast ring wrapped and `n` agent events were never delivered.
    /// Surfaced as a note instead of silently rendering a truncated stream.
    EventsLost(u64),
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
    /// Images staged for the next submit. `alt+v` reads the clipboard into this
    /// list; the composer shows a chip per entry. Kept beside `input` rather than
    /// inside it so the text buffer stays a plain `String` (the composer, history
    /// navigation and `/command` parsing all assume that).
    pending_images: Vec<clipboard_image::ClipboardImage>,
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
            pending_images: Vec::new(),
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
    /// Is a modal window covering the composer this frame?
    ///
    /// The set was previously spelled out as a seven-term `||` chain at every
    /// call site, which meant adding a window required editing each one and
    /// risking a stale chain that lets keys leak into the transcript.
    pub(crate) fn modal_open(&self) -> bool {
        self.show_models
            || self.show_login
            || self.show_settings
            || self.show_thinking_picker
            || self.renderer_picker.is_some()
            || self.request_picker.is_some()
            || self.picker.is_some()
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
        if new_session {
            // Prompts queued for the abandoned conversation must not follow the
            // user into a fresh one; `install_agent` adopts the inbox as-is.
            self.input_queue.clear();
        }
        let result = if new_session {
            wiring::make_agent(&self.root.clone(), &self.settings, false)
                .map_err(anyhow::Error::from)
        } else {
            self.reopen_session()
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

    /// Reopen the current session.
    ///
    /// This used to retry for up to a second while the dropped agent's redb
    /// lock drained, because a second `SnapshotStore::open` on a live session
    /// failed with "database already open". Snapshot databases are now shared
    /// process-wide through a `Weak` cache, so reopening reuses the live handle
    /// and there is nothing to wait for.
    fn reopen_session(&mut self) -> anyhow::Result<(Agent, Arc<feather::tasks::TaskStore>)> {
        let session =
            feather_core::session::Session::open(&self.session_dir, &config::feather_home())
                .map_err(|e| anyhow::anyhow!("reopen session: {e}"))?;
        wiring::make_agent_for_session(&self.root.clone(), &self.settings, session)
            .map_err(anyhow::Error::from)
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
        // A different conversation must not inherit prompts queued for the
        // previous one; `install_agent` adopts whatever is still in the inbox.
        self.input_queue.clear();
        self.session_dir = dir;
        match self.reopen_session() {
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
            // The new agent owns a brand-new inbox. Carry over anything the
            // user already queued, or a model switch mid-run silently eats it.
            let pending = std::mem::take(&mut self.input_queue);
            self.input_queue = guard.input_queue();
            self.input_queue.adopt(&pending);
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
        self.sync_queued_hint();
    }

    /// Mirror the inbox's real depth into the UI counter.
    ///
    /// Everything that queues or delivers a prompt funnels through here. Keeping
    /// a hand-maintained counter instead let the hint advertise follow-ups that
    /// no longer existed — which reads as "queued messages never start".
    fn sync_queued_hint(&mut self) {
        self.queued_hint = self.input_queue.pending();
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
    /// Stage an image from the clipboard for the next submit.
    ///
    /// The three outcomes are deliberately distinct, because "nothing happened"
    /// was the previous behaviour of the `alt+v` binding: no image on the
    /// clipboard, a clipboard image that could not be decoded, and success all
    /// have to look different to the user.
    fn paste_image(&mut self) {
        if self.exit.is_some() {
            return;
        }
        match clipboard_image::read_image() {
            Some(image) => {
                let (w, h) = (image.width, image.height);
                self.pending_images.push(image);
                self.rows.push(ChatRow::new(
                    RowKind::Notice,
                    format!("Attached clipboard image {w}x{h}"),
                ));
            }
            None => {
                let reason = clipboard_image::last_error()
                    .unwrap_or_else(|| "no image on the clipboard".into());
                self.rows.push(ChatRow::new(RowKind::Notice, reason));
            }
        }
        self.egui_ctx.request_repaint();
    }

    /// Move the composer text and any staged images into one message.
    ///
    /// Every send path funnels through here so an attachment can never be
    /// silently dropped by a path that only knew about `String`.
    fn take_prompt_message(&mut self, text: String) -> feather_core::message::Message {
        let mut message = feather_core::message::Message::user_text(text);
        for image in std::mem::take(&mut self.pending_images) {
            message
                .content
                .push(feather_core::message::ContentBlock::Image {
                    media_type: image.media_type,
                    data: image.data,
                });
        }
        message
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
        // An image alone is a complete prompt: attaching a screenshot and pressing
        // enter should send it, without requiring a caption.
        if text.is_empty() && self.pending_images.is_empty() {
            return;
        }
        self.input.clear();
        if self.running {
            // The thread-safe inbox remains writable while run() owns Agent.
            let message = self.take_prompt_message(text);
            self.input_queue.push(message, false);
            self.sync_queued_hint();
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
        // The GUI drives the agent in-process and never passes through
        // `rpc::check_image_support`, so the same modality rule has to be applied
        // here: sending an image to a text-only model costs a whole run to learn.
        // An unknown model is still allowed to try, matching the RPC behaviour.
        if !self.pending_images.is_empty() {
            if let Some(false) = feather::models::accepts_images(&self.settings.model) {
                let model = self.settings.model.clone();
                self.rows.push(ChatRow::new(
                    RowKind::Error,
                    format!(
                        "model `{model}` does not accept image input; remove the attachment \
                         or switch to a vision-capable model"
                    ),
                ));
                return;
            }
        }
        // The row mirrors the staged attachments so the picture appears in the
        // transcript the moment it is sent, not only after a reload.
        let mut user_row = ChatRow::new(RowKind::User, text.clone());
        user_row.images = self
            .pending_images
            .iter()
            .map(|image| RowImage {
                media_type: image.media_type.clone(),
                data: image.data.clone(),
                texture: None,
                failed: false,
            })
            .collect();
        self.rows.push(user_row);
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
        // Build the message once, at the only point where it is certain to be
        // sent: earlier failures above return the text to the composer, and the
        // attachments must survive that rollback rather than being taken and lost.
        let message = self.take_prompt_message(text);
        self.rt.spawn(drive_prompt(agent, message, tx, ctx));
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
                if self.modal_open() {
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
                // An image alone can be a follow-up too, so the same rule as
                // `submit` applies: text OR a staged attachment makes it sendable.
                if (!text.is_empty() || !self.pending_images.is_empty()) && self.running {
                    let message = self.take_prompt_message(text);
                    self.input_queue.push(message, false);
                    self.input.clear();
                    self.sync_queued_hint();
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
                                    self.sync_queued_hint();
                                    let draft = std::mem::replace(&mut self.input, message.text());
                                    self.submit();
                                    self.input = draft;
                                }
                            }
                            // A failed run deliberately does not retry: hammering
                            // an exhausted quota would just fail again. Say so, or
                            // the still-queued prompts look dropped.
                            if matches!(r.stop_reason, StopReason::Error)
                                && self.input_queue.pending() > 0
                            {
                                let count = self.input_queue.pending();
                                self.rows.push(ChatRow::new(
                                    RowKind::Notice,
                                    format!(
                                        "Kept {count} queued prompt(s). They will start \
                                         automatically after your next successful turn — \
                                         switch model or send a message to continue."
                                    ),
                                ));
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
                UiEvent::EventsLost(n) => {
                    // The delta stream is incomplete, so text and tool rows may be
                    // missing from what the UI accumulated. The session is always
                    // authoritative: rebuild the rows from it rather than leaving
                    // a silently truncated transcript on screen.
                    if let Some(agent) = self.agent.clone() {
                        if let Ok(guard) = agent.try_lock() {
                            self.rows = rows_from_messages(guard.session().messages());
                        }
                    }
                    self.rows.push(ChatRow::new(
                        RowKind::Notice,
                        format!(
                            "The event stream fell behind and {n} updates were dropped; \
                             the transcript above was rebuilt from the session."
                        ),
                    ));
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
                    self.sync_queued_hint();
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
                // No `details` yet; anchor tools hide their opaque anchors here
                // and get the real path once ToolEnd reports it.
                row.tool_args = tool_subject(&call.name, &call.arguments, None, None)
                    .unwrap_or_else(|| tool_summary(&call.arguments));
                row.tool_detail = tool_detail(&call.name, &call.arguments);
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
                details,
            } => {
                let text = truncate_owned(text, 20_000);
                // The tool reports the file it edited; the anchor arguments in
                // `tool_args` cannot name a file, so the card would otherwise
                // show only opaque anchors.
                let resolved =
                    tool_subject(&call.name, &call.arguments, details.as_ref(), Some(&text));
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
                    // ToolStart ran before the tool knew its target, so upgrade
                    // the summary now that the real subject is known.
                    if let Some(subject) = &resolved {
                        row.tool_args = subject.clone();
                    }
                } else {
                    let mut row = ChatRow::new(RowKind::Tool, &call.name);
                    row.tool_id = Some(call.id);
                    row.tool_args = resolved.unwrap_or_else(|| tool_summary(&call.arguments));
                    row.tool_detail = tool_detail(&call.name, &call.arguments);
                    row.tool_output = Some(text);
                    row.tool_error = is_error;
                    self.rows.push(row);
                }
                self.history.durable_tail_changed();
            }
            AgentEvent::ToolProgress { call, text } => {
                let text = truncate_owned(text, 20_000);
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
    message: feather_core::message::Message,
    tx: Sender<UiEvent>,
    ctx: egui::Context,
) {
    let mut guard = agent.lock().await;
    let mut events = guard.subscribe();
    let mut prompt = Box::pin(guard.prompt_message_prepared(message));
    let result = loop {
        tokio::select! {
            result=&mut prompt=>break result.map_err(|e|e.to_string()),
            event=events.recv()=>match event {
                Ok(event)=>{if tx.send(UiEvent::Agent(event)).is_err(){return;}ctx.request_repaint_after(std::time::Duration::from_millis(33));},
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n))=>{if tx.send(UiEvent::EventsLost(n)).is_err(){return;}},
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
            // The ring wrapped: some deltas were never forwarded. Silently
            // discarding the loss renders as a gap with no explanation, so the
            // UI is told and can resync the transcript from the session instead
            // of trusting the delta stream.
            Err(tokio::sync::broadcast::error::TryRecvError::Lagged(n)) => {
                if tx.send(UiEvent::EventsLost(n)).is_err() {
                    return;
                }
                continue;
            }
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
    truncate_owned(text.to_string(), limit)
}

/// Same truncation for a caller that already owns the text.
///
/// `truncate_utf8(&event_text, ..)` would copy the whole tool result only to
/// drop the original; this truncates in place and returns the input untouched
/// when it is already under the limit.
fn truncate_owned(mut text: String, limit: usize) -> String {
    if text.len() <= limit {
        return text;
    }
    let mut end = limit;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
    text.push_str("\n… (truncated)");
    text
}

/// Truncate to `max_chars` **characters**, matching the unit the agent uses.
///
/// The tool layer caps output with a character budget (`feather::bash`), so a
/// byte budget here cuts a multi-byte result earlier than the model's own limit
/// and the UI shows strictly less than the model was given. Counting characters
/// keeps the two views consistent for CJK and emoji output, where a byte limit is
/// up to 3x stricter than the agent's.
fn truncate_chars(text: String, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text;
    }
    let mut out: String = text.chars().take(max_chars.saturating_sub(1)).collect();
    out.push('…');
    out
}

fn tool_summary(args: &serde_json::Value) -> String {
    for key in ["path", "command", "pattern", "task", "url"] {
        if let Some(value) = args.get(key).and_then(|v| v.as_str()) {
            return truncate_utf8(value.lines().next().unwrap_or(value), 180);
        }
    }
    feather_core::tool::tool_args_summary(args, 180)
}

/// Must match `FILE_HEADER` in `feather-hashline`: the first line of an
/// editing tool's output names the file it touched.
const TOOL_FILE_HEADER: &str = "file: ";

/// The edited file, read back out of a tool's output text.
///
/// Editing tools name the file on line one so a card rebuilt from the persisted
/// transcript can still say *which* file changed — the anchor arguments never
/// carry a path, and the `details` side-channel is not stored.
fn tool_output_path(output: &str) -> Option<&str> {
    let first = output.lines().next()?;
    first
        .strip_prefix(TOOL_FILE_HEADER)
        .map(str::trim)
        .filter(|p| !p.is_empty())
}

/// What a tool call is actually about, in words a reader recognizes.
///
/// Returns `None` when the plain argument summary is already the clearest
/// thing to show. Two cases need help:
///
/// * Anchor-addressed edits (`replace`, `replace_within`, `insert`, `copy`,
///   `move`, `undo_last_change`) carry only 5-letter anchors, so the default
///   summary reads `remove_from ab3cd, remove_to xy9zw` — it never names the
///   file. The tool now reports the resolved path in `details`.
/// * `bash` commands are often long; collapsing them to the first line and
///   appending an ellipsis keeps the card one row tall without hiding that
///   more text exists.
fn tool_subject(
    name: &str,
    args: &serde_json::Value,
    details: Option<&serde_json::Value>,
    output: Option<&str>,
) -> Option<String> {
    let reported = details
        .and_then(|d| d.get("path"))
        .and_then(|p| p.as_str())
        .filter(|p| !p.is_empty())
        // The structured channel is absent when a card is rebuilt from the
        // transcript, but the tool writes the same path into its first line.
        .or_else(|| output.and_then(tool_output_path));

    match name {
        // Anchor tools: the file is the useful subject, anchors are not.
        "replace" | "replace_within" | "insert" | "copy" | "move" | "undo_last_change" => {
            let Some(path) = reported.map(str::to_string).or_else(|| {
                args.get("path")
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
            }) else {
                // Path unknown yet (ToolStart, or a failed edit). Show nothing
                // rather than dumping `remove_from ab3cd, remove_to xy9zw`.
                return Some(String::new());
            };
            Some(truncate_utf8(path.trim_start_matches("./"), 180))
        }
        // Only the model's stated intent is shown; the command itself is not.
        "bash" | "shell" => {
            let intent = args
                .get("description")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty());
            match intent {
                Some(intent) => Some(truncate_utf8(intent, 180)),
                // The tool now refuses a call without an intent, so this is only
                // reached by rows recorded before that rule existed. Showing the
                // raw command here is what made collapsed cards unreadable, so an
                // old row shows its tool name and nothing else; the command stays
                // one click away under COMMAND.
                None => Some(String::new()),
            }
        }
        // A delegated run is defined by its label and its model: the task text
        // is too long for one line, and without the model two concurrent
        // children look identical. `details.model` is authoritative once the
        // child resolves it; before that, the requested id is the best answer.
        "subagent" | "agent" => {
            let label = args
                .get("label")
                .or_else(|| args.get("description"))
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty());
            let model = details
                .and_then(|d| d.get("model"))
                .and_then(|m| m.as_str())
                .filter(|m| !m.is_empty())
                .or_else(|| args.get("model").and_then(|m| m.as_str()));
            match (label, model) {
                (Some(label), Some(model)) => {
                    Some(truncate_utf8(&format!("{label} · {model}"), 180))
                }
                (Some(label), None) => Some(truncate_utf8(label, 180)),
                (None, Some(model)) => Some(truncate_utf8(model, 180)),
                // Nothing resolved yet: an empty subject beats echoing the task.
                (None, None) => Some(String::new()),
            }
        }
        _ => None,
    }
}

/// Raw arguments worth revealing under the card, paired with [`tool_subject`].
///
/// The header can only hold one line, so it shows the *meaning* (the file, or
/// the model's stated intent). The full command or anchor list is still
/// available on demand rather than lost.
fn tool_detail(name: &str, args: &serde_json::Value) -> String {
    match name {
        "bash" | "shell" => args
            .get("command")
            .and_then(|v| v.as_str())
            .map(|c| c.trim().to_string())
            .unwrap_or_default(),
        "replace" | "replace_within" | "insert" | "copy" | "move" | "undo_last_change" => {
            feather_core::tool::tool_args_summary(args, 300)
        }
        // The delegated task is the whole point of the call, and it cannot fit
        // in the one-line header. Reveal it here instead of hiding it.
        "subagent" | "agent" => args
            .get("task")
            .or_else(|| args.get("prompt"))
            .and_then(|v| v.as_str())
            .map(|t| t.trim().to_string())
            .unwrap_or_default(),
        _ => String::new(),
    }
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
                // An image-only turn has no text, so gating on text alone dropped
                // the attachment entirely on reload. Attachments decide the row.
                let images: Vec<RowImage> = message
                    .content
                    .iter()
                    .filter_map(|block| match block {
                        feather_core::ContentBlock::Image { media_type, data } => Some(RowImage {
                            media_type: media_type.clone(),
                            data: data.clone(),
                            texture: None,
                            failed: false,
                        }),
                        _ => None,
                    })
                    .collect();
                if !text.is_empty() || !images.is_empty() {
                    let mut row = ChatRow::new(RowKind::User, text);
                    row.images = images;
                    rows.push(row);
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
                    // The result text arrives in a later message; it carries the
                    // path, so this is filled in again when ToolResult is seen.
                    row.tool_args = tool_subject(&call.name, &call.arguments, None, None)
                        .unwrap_or_else(|| tool_summary(&call.arguments));
                    row.tool_detail = tool_detail(&call.name, &call.arguments);
                    rows.push(row);
                }
            }
            Role::ToolResult => {
                // Characters, not bytes: this is the row the user compares against
                // the model's own char-budgeted view of the same tool result.
                let output = truncate_chars(message.text(), 20_000);
                let id = message.tool_call_id.clone();
                if let Some(row) = rows
                    .iter_mut()
                    .rev()
                    .find(|row| row.tool_id == id && row.tool_output.is_none())
                {
                    // The result names the edited file on its first line; use it
                    // so a reloaded card shows the path exactly like a live one.
                    if let Some(path) = tool_output_path(&output) {
                        if row.tool_args.is_empty() {
                            row.tool_args = truncate_utf8(path.trim_start_matches("./"), 180);
                        }
                    }
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
