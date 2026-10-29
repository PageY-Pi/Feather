//! Linear session: raw audit history + metadata on redb (`session.redb`) with
//! turn-granularity snapshots.
//!
//! Differences from pi by design:
//! - **linear, not a tree** — no branches, no `parentId`; context is the
//!   plain message list (requested feature: 线性上下文).
//! - **turn-granularity snapshots** — every completed turn commits a
//!   snapshot in a redb store; `/rewind N` restores the message list from
//!   the snapshot N turns back.
//! - the raw audit history lives in `session.redb` (see
//!   [`crate::session_store`]) and keeps every pushed message, including
//!   pre-compaction and pre-shrink tool results; model context is rebuilt from
//!   the linear list where compaction may have shrunk tool results. Legacy
//!   `session.jsonl` files are migrated on open.

use crate::message::{Message, Usage};
use crate::session_store::{self, RawEntry, SessionMeta, SessionStore};
use crate::snapshot::SnapshotStore;
use std::path::{Path, PathBuf};

pub struct Session {
    id: String,
    cwd: PathBuf,
    dir: PathBuf,
    /// Feather home the session belongs to (empty for in-memory sessions);
    /// used to refresh the discovery catalog.
    home: PathBuf,
    store: Option<SessionStore>,
    child_store: Option<(std::sync::Arc<crate::substore::SubagentStore>, String)>,
    child_seq: u64,
    messages: std::sync::Arc<Vec<Message>>,
    context_tokens: u64,
    history_hash: u64,
    message_hashes: Vec<u64>,
    prefix_tokens: Vec<u64>,
    snapshots: Option<SnapshotStore>,
    turn: u64,
    created_ms: u64,
    first_prompt: String,
    /// Cumulative usage across the whole session (incl. compaction calls).
    total_usage: Usage,
    /// Host-side problems the *model* has to know about (skipped checkpoint
    /// paths). Injected into the context as a `<feather-runtime-diagnostic>`
    /// user message on the next turn.
    runtime_diagnostics: Vec<String>,
    /// Repair bookkeeping the *user* has to know about. Never enters the model
    /// context: a repaired call/result pairing is not something to instruct a
    /// model about, it is a notice that the session was healed.
    notes: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("snapshot: {0}")]
    Snapshot(#[from] crate::snapshot::SnapshotError),
    #[error("session store: {0}")]
    Store(#[from] session_store::SessionStoreError),
    #[error("catalog: {0}")]
    Catalog(String),
    #[error("git checkpoint: {0}")]
    Git(String),
}

/// One-line note for a repaired context. It rides [`Session::notes`], which is
/// flushed to the UI as a Notice on the next committed turn — user-visible,
/// never fed back to the model — so a repaired session is not silently
/// rewritten.
fn repaired_context_diagnostic(changed: usize, source: &str) -> String {
    format!(
        "context repaired ({source}): {changed} tool message(s) re-paired so the next request is \
         accepted; the raw transcript keeps everything"
    )
}

impl Session {
    /// Create a fresh session under `<feather_home>/sessions/`.
    pub fn new(feather_home: &Path, cwd: &Path) -> Result<Self, SessionError> {
        let id = crate::provider::gen_id("s");
        let dir = feather_home.join("sessions").join(&id);
        std::fs::create_dir_all(&dir)?;
        let snapshot_dir = dir.join("snapshots");
        let snapshots = SnapshotStore::open(&snapshot_dir)?;
        let store = SessionStore::open(&dir)?;
        let now = crate::message::now_millis();
        let meta = SessionMeta {
            id: id.clone(),
            cwd: cwd.to_path_buf(),
            created_ms: now,
            modified_ms: now,
            first_prompt: String::new(),
        };
        store.write_meta(&meta)?;
        let session = Self {
            id,
            cwd: cwd.to_path_buf(),
            dir,
            home: feather_home.to_path_buf(),
            store: Some(store),
            child_store: None,
            child_seq: 0,
            messages: std::sync::Arc::new(Vec::new()),
            context_tokens: 16,
            history_hash: crate::message::fnv1a(b"feather-history-v1"),
            message_hashes: Vec::new(),
            prefix_tokens: vec![16],
            snapshots: Some(snapshots),
            turn: 0,
            created_ms: now,
            first_prompt: String::new(),
            total_usage: Usage::default(),
            runtime_diagnostics: Vec::new(),
            notes: Vec::new(),
        };
        session.register_catalog()?;
        Ok(session)
    }

    /// In-memory child session: no session redb or snapshot files are created.
    /// Each pushed message streams to the supplied parent-owned child redb store.
    pub fn child(
        cwd: &Path,
        id: String,
        store: std::sync::Arc<crate::substore::SubagentStore>,
    ) -> Self {
        let mut session = Self::memory(cwd, id.clone());
        session.child_store = Some((store, id));
        session
    }
    pub fn memory(cwd: &Path, id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            cwd: cwd.to_path_buf(),
            dir: PathBuf::new(),
            home: PathBuf::new(),
            store: None,
            child_store: None,
            child_seq: 0,
            messages: std::sync::Arc::new(Vec::new()),
            context_tokens: 16,
            history_hash: crate::message::fnv1a(b"feather-history-v1"),
            message_hashes: Vec::new(),
            prefix_tokens: vec![16],
            snapshots: None,
            turn: 0,
            created_ms: crate::message::now_millis(),
            first_prompt: String::new(),
            total_usage: Usage::default(),
            runtime_diagnostics: Vec::new(),
            notes: Vec::new(),
        }
    }

    /// Reopen the most recent session for `cwd` (used by `/resume`).
    pub fn resume_latest(feather_home: &Path, cwd: &Path) -> Result<Option<Self>, SessionError> {
        for info in crate::session_catalog::list_sessions(feather_home, cwd)? {
            match Self::open(&info.dir, feather_home) {
                Ok(session) => return Ok(Some(session)),
                Err(e) => {
                    // silently skipping a broken newer session would resume an
                    // OLDER one with no explanation
                    eprintln!("feather: skipping session {}: {e}", info.dir.display());
                }
            }
        }
        Ok(None)
    }

    /// Open an existing session directory, migrating a legacy `session.jsonl`
    /// first (atomic + verified; the JSONL is only retired afterwards).
    pub fn open(dir: &Path, feather_home: &Path) -> Result<Self, SessionError> {
        session_store::import_legacy(dir)?;
        let store = SessionStore::open(dir)?;
        let meta = match store.read_meta()? {
            Some(m) => m,
            None => {
                // A store created outside `Session::new` (or by a crash before
                // the metadata txn): synthesize identity from the directory.
                let id = dir
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                let now = crate::message::now_millis();
                let m = SessionMeta {
                    id,
                    cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
                    created_ms: now,
                    modified_ms: now,
                    first_prompt: String::new(),
                };
                store.write_meta(&m)?;
                m
            }
        };
        let id = meta.id.clone();
        let cwd = if meta.cwd.as_os_str().is_empty() {
            std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
        } else {
            meta.cwd.clone()
        };
        let first_prompt = if meta.first_prompt.is_empty() {
            let page = store.raw_page_before(Some(64), 64, 256 * 1024)?;
            session_store::first_prompt_from(page.entries.iter().map(|e| &e.message))
        } else {
            meta.first_prompt.clone()
        };

        let snapshots = SnapshotStore::open(&dir.join("snapshots"))?;
        let turn = snapshots.current_turn()?;
        // Rebuild the linear list from the durable current snapshot when
        // available (crash-resume: the tail after the snapshot is discarded).
        let mut messages = if turn > 0 {
            match snapshots.load(turn) {
                Ok(m) => m,
                Err(e) => {
                    // silent fallback would resurrect turns that compaction
                    // or /rewind already discarded — at least make it visible
                    eprintln!(
                        "feather: snapshot load failed for turn {turn} ({e}); \
                         falling back to raw history replay"
                    );
                    store.raw_messages()?
                }
            }
        } else {
            Vec::new()
        };
        // A snapshot written by a build whose compaction cut between a tool
        // call and its result reopens every request as a 400 (a `tool` message
        // with no `tool_calls` assistant before it). Repair on load, once, so
        // the session is usable instead of bricked.
        let mut notes = Vec::new();
        let repaired = crate::compaction::repair_tool_pairs(&mut messages);
        if repaired > 0 {
            notes.push(repaired_context_diagnostic(repaired, "resume"));
        }
        let mut total_usage = Usage::default();
        for m in &messages {
            if let Some(u) = m.usage {
                total_usage = add_usage(total_usage, u);
            }
        }
        let mut session = Self {
            id,
            cwd,
            dir: dir.to_path_buf(),
            home: feather_home.to_path_buf(),
            store: Some(store),
            child_store: None,
            child_seq: 0,
            context_tokens: 16,
            message_hashes: Vec::new(),
            prefix_tokens: Vec::new(),
            history_hash: crate::message::fnv1a(b"feather-history-v1"),
            messages: std::sync::Arc::new(messages),
            snapshots: Some(snapshots),
            turn,
            created_ms: meta.created_ms,
            first_prompt,
            total_usage,
            runtime_diagnostics: Vec::new(),
            notes,
        };
        session.refresh_context_metrics();
        session.register_catalog()?;
        Ok(session)
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Absolute path of the session's redb storage (`<dir>/session.redb`), the
    /// value the RPC/GUI surface as `sessionFile`. `None` for memory sessions.
    pub fn session_file(&self) -> Option<&Path> {
        self.store.as_ref().map(SessionStore::path)
    }

    pub fn turn(&self) -> u64 {
        self.turn
    }

    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    /// Immutable request context: one Arc increment, no deep history clone.
    /// Requests must be dropped before pushing to keep the copy-on-write path
    /// unique; external snapshots remain valid if a caller retains a handle.
    pub fn shared_messages(&self) -> std::sync::Arc<Vec<Message>> {
        std::sync::Arc::clone(&self.messages)
    }
    pub fn context_tokens(&self) -> u64 {
        self.context_tokens
    }
    pub fn history_hash(&self) -> u64 {
        self.history_hash
    }
    pub fn message_hashes(&self) -> &[u64] {
        &self.message_hashes
    }
    pub fn prefix_tokens(&self) -> &[u64] {
        &self.prefix_tokens
    }
    fn refresh_context_metrics(&mut self) {
        self.context_tokens = 16;
        self.history_hash = crate::message::fnv1a(b"feather-history-v1");
        self.message_hashes.clear();
        self.prefix_tokens.clear();
        self.prefix_tokens.push(16);
        for message in self.messages.iter() {
            let hash =
                crate::message::json_hash(message).expect("Message serialization is infallible");
            self.context_tokens += crate::tokenizer::estimate_message_tokens(message);
            self.history_hash = self.history_hash.rotate_left(1) ^ hash;
            self.message_hashes.push(hash);
            self.prefix_tokens.push(self.context_tokens);
        }
    }

    pub fn snapshots(&self) -> Option<&SnapshotStore> {
        self.snapshots.as_ref()
    }

    pub fn total_usage(&self) -> Usage {
        self.total_usage
    }

    /// Independent read handle for UI audit paging. It shares the existing
    /// redb database and never waits for the Agent mutex during a live run.
    pub fn transcript_store(&self) -> Option<SessionStore> {
        self.store.clone()
    }

    /// Raw audit history (every pushed message, pre-compaction originals
    /// included). Replaces reading the old JSONL transcript.
    pub fn raw_messages(&self) -> Result<Vec<Message>, SessionError> {
        match self.store.as_ref() {
            Some(s) => Ok(s.raw_messages()?),
            None => Ok(Vec::new()),
        }
    }

    /// Raw audit history with entry ids/timestamps, in push order.
    pub fn raw_entries(&self) -> Result<Vec<RawEntry>, SessionError> {
        match self.store.as_ref() {
            Some(s) => Ok(s.raw_entries()?),
            None => Ok(Vec::new()),
        }
    }

    pub fn add_usage(&mut self, u: Usage) {
        self.total_usage = add_usage(self.total_usage, u);
    }

    pub fn take_runtime_diagnostics(&mut self) -> Vec<String> {
        std::mem::take(&mut self.runtime_diagnostics)
    }
    /// User-facing notes (currently: a repaired context). Never injected into
    /// the model context — see [`Session::runtime_diagnostics`] for the
    /// provider-visible channel.
    pub fn take_notes(&mut self) -> Vec<String> {
        std::mem::take(&mut self.notes)
    }
    /// Append a message to the linear list and the raw redb audit history.
    pub fn push(&mut self, msg: Message) -> Result<(), SessionError> {
        let message_hash = crate::message::json_hash(&msg)?;
        if let Some((store, id)) = &self.child_store {
            store
                .push_message(id, self.child_seq, &msg)
                .map_err(|e| SessionError::Catalog(e.to_string()))?;
        }
        if let Some(store) = self.store.as_ref() {
            let is_first_prompt =
                self.first_prompt.is_empty() && msg.role == crate::message::Role::User;
            let first_prompt = if is_first_prompt {
                Some(crate::session_store::first_prompt_from(std::iter::once(
                    &msg,
                )))
            } else {
                None
            };
            store.append_raw_with_meta(
                &crate::provider::gen_id("e"),
                msg.timestamp,
                &msg,
                crate::message::now_millis(),
                first_prompt.as_deref().unwrap_or(&self.first_prompt),
            )?;
            if let Some(first_prompt) = first_prompt {
                self.first_prompt = first_prompt;
            }
            if is_first_prompt {
                // Make the prompt visible to discovery immediately, before the
                // first turn commits.
                self.register_catalog()?;
            }
        }
        self.child_seq += 1;
        self.context_tokens += crate::tokenizer::estimate_message_tokens(&msg);
        self.history_hash = self.history_hash.rotate_left(1) ^ message_hash;
        self.message_hashes.push(message_hash);
        self.prefix_tokens.push(self.context_tokens);
        std::sync::Arc::make_mut(&mut self.messages).push(msg);
        Ok(())
    }

    /// Commit the current message list as a snapshot for the given turn and
    /// bump the turn counter.
    pub fn commit_turn(&mut self) -> Result<u64, SessionError> {
        let next_turn = self.turn + 1;
        if self.snapshots.is_none() {
            self.turn = next_turn;
            return Ok(self.turn);
        }
        // Working-directory checkpoints are best-effort diagnostics now: a git
        // index/path failure must not abort the prompt or prevent the durable
        // message snapshot from advancing. Rewind still has file_snapshots for
        // anchor-tool edits; git remains an auxiliary workspace restore path.
        let (_, writes) = crate::compaction::file_ops(&self.messages);
        let owned = crate::file_snapshots::FileSnapshots::new(&self.dir)
            .tracked_paths()
            .unwrap_or_default();
        let mut files = writes
            .into_iter()
            .filter(|file| {
                let raw = crate::workspace_path::normalize_input(Path::new(file));
                let full = if raw.is_absolute() {
                    raw
                } else {
                    self.cwd.join(raw)
                };
                !owned.iter().any(|path| {
                    crate::workspace_path::comparison_key(path)
                        == crate::workspace_path::comparison_key(&full)
                })
            })
            .collect::<Vec<_>>();
        for tracker in ["TODO.md", "README.md"] {
            if !files.iter().any(|f| f == tracker) {
                files.push(tracker.to_string());
            }
        }
        match crate::gitckpt::GitCheckpoints::open(&self.cwd)
            .commit_turn_report(next_turn, &self.dir, &files)
        {
            Ok(report) => self.runtime_diagnostics.extend(report.diagnostics),
            Err(e) => self.runtime_diagnostics.push(format!(
                "workspace checkpoint skipped; durable session snapshot continued: {e}"
            )),
        }
        if !self.runtime_diagnostics.is_empty() {
            // Diagnostic is provider-visible data, not a user instruction. Keep
            // it in the same durable snapshot so reopen cannot lose the reason
            // a checkpoint/path was skipped. Tool errors already persist as
            // ToolResult; internal host errors need this explicit bridge.
            let diagnostics = self
                .runtime_diagnostics
                .iter()
                .take(16)
                .map(|d| d.chars().take(1200).collect::<String>())
                .collect::<Vec<_>>()
                .join("\n");
            let text=format!("<feather-runtime-diagnostic>\n{diagnostics}\nSome checkpoint paths or operations were skipped (see reasons above). Continue with an allowed alternative; do not retry the same illegal operation.\n</feather-runtime-diagnostic>");
            let duplicate = self.messages.last().is_some_and(|m| m.text_cow() == text);
            if !duplicate {
                self.push(Message::user_text(text))?;
            }
        }
        crate::file_snapshots::FileSnapshots::new(&self.dir).commit(next_turn)?;
        self.snapshots
            .as_ref()
            .unwrap()
            .snapshot(next_turn, &self.messages)?;
        self.turn = next_turn;
        if let Some(store) = self.store.as_ref() {
            store.touch(crate::message::now_millis(), &self.first_prompt)?;
        }
        self.register_catalog()?;
        Ok(self.turn)
    }

    /// Restore the message list from `turn` AND the working directory from
    /// that turn's git checkpoint (managed repos get an exact tree restore;
    /// pre-existing repos get content restore only). Raw audit history keeps
    /// every message.
    pub fn rewind_to(&mut self, turn: u64) -> Result<Vec<Message>, SessionError> {
        let Some(snapshots) = self.snapshots.as_ref() else {
            return Err(SessionError::Snapshot(
                crate::snapshot::SnapshotError::Table(
                    "memory session has no rewind snapshots".into(),
                ),
            ));
        };
        let msgs = snapshots.load(turn)?;
        let workspace = crate::gitckpt::GitCheckpoints::open(&self.cwd);
        let file_store = crate::file_snapshots::FileSnapshots::new(&self.dir);
        let excluded = file_store.tracked_paths().unwrap_or_default();
        crate::file_snapshots::FileSnapshots::new(&self.dir).restore_with(turn, || {
            if workspace.has_checkpoint(turn, &self.dir) {
                workspace.restore_excluding(turn, &self.dir, &excluded)
            } else {
                Ok(())
            }
        })?;
        // Durable current pointer: reopening after this rewind (with no further
        // prompt) restores `turn`, not the newest snapshot the store still
        // holds for forward history.
        snapshots.set_current(turn)?;
        // A snapshot from before the repair fix can carry a tool result whose
        // call is gone. Rewind restores the snapshot verbatim (that is its
        // contract, and `/rewind N` must show exactly what turn N held); the
        // repair runs before the next request instead — see
        // [`Session::repair_context`] and its call sites.
        let discarded = std::mem::replace(&mut self.messages, std::sync::Arc::new(msgs));
        self.refresh_context_metrics();
        self.turn = turn;
        Ok(std::sync::Arc::try_unwrap(discarded).unwrap_or_else(|shared| shared.as_ref().clone()))
    }

    pub fn latest_snapshot_turn(&self) -> Result<u64, SessionError> {
        Ok(self
            .snapshots
            .as_ref()
            .map(|s| s.latest_turn())
            .transpose()?
            .unwrap_or(self.turn))
    }

    /// Kernel-internal seam used by compaction: swap the linear list without
    /// touching the raw audit history (originals stay durable in redb).
    pub fn set_messages_for_compaction(&mut self, mut msgs: Vec<Message>) {
        // Compaction is the one place that rewrites the whole context. Install
        // it only in a shape providers accept: a kept span that opens on a tool
        // result (call summarized away) makes every later request a 400.
        let repaired = crate::compaction::repair_tool_pairs(&mut msgs);
        if repaired > 0 {
            self.notes
                .push(repaired_context_diagnostic(repaired, "compaction"));
        }
        self.messages = std::sync::Arc::new(msgs);
        self.refresh_context_metrics();
    }

    /// Repair tool call/result pairing on the live context. Idempotent, and a
    /// no-op that does not touch the messages when they are already valid, so
    /// callers can run it before every request.
    pub fn repair_context(&mut self, source: &str) {
        if !crate::compaction::has_broken_tool_pairs(&self.messages) {
            return;
        }
        // Only pay for the copy when there is something to repair: the live
        // history is shared with in-flight requests, and cloning it on every
        // prompt is exactly the copy-on-write this list is designed to avoid.
        let mut messages = match std::sync::Arc::try_unwrap(std::mem::replace(
            &mut self.messages,
            std::sync::Arc::new(Vec::new()),
        )) {
            Ok(owned) => owned,
            Err(shared) => shared.as_ref().clone(),
        };
        let repaired = crate::compaction::repair_tool_pairs(&mut messages);
        self.messages = std::sync::Arc::new(messages);
        if repaired > 0 {
            self.notes
                .push(repaired_context_diagnostic(repaired, source));
        }
        self.refresh_context_metrics();
    }

    /// pi's /fork: copy the whole session (raw history, snapshots) to a fresh id
    /// and continue in the copy. The original stays untouched on disk.
    pub fn fork(&self) -> Result<Self, SessionError> {
        // rebuild instead of copying: session.redb / snapshots.redb are held
        // open (locked on Windows), and a fresh store is cleaner than a copied
        // one anyway. The git checkpoint map (turn → commit) is deliberately
        // NOT carried over — the fork starts its own working-dir history.
        let home = if self.home.as_os_str().is_empty() {
            self.dir
                .parent()
                .and_then(|p| p.parent())
                .ok_or_else(|| SessionError::Io(std::io::Error::other("session dir has no home")))?
                .to_path_buf()
        } else {
            self.home.clone()
        };
        let mut s = Session::new(&home, &self.cwd)?;
        for m in self.messages() {
            s.push(m.clone())?;
        }
        s.turn = self.turn;
        s.total_usage = self.total_usage;
        s.first_prompt = self.first_prompt.clone();
        if self.turn > 0 {
            s.snapshots
                .as_ref()
                .unwrap()
                .snapshot(self.turn, &s.messages)?;
        }
        if !self.dir.as_os_str().is_empty() {
            crate::file_snapshots::FileSnapshots::new(&self.dir).fork_into(&s.dir)?;
        }
        s.register_catalog()?;
        Ok(s)
    }

    /// Refresh the discovery catalog with this session's metadata. Best-effort
    /// for memory sessions (no home).
    fn register_catalog(&self) -> Result<(), SessionError> {
        if self.home.as_os_str().is_empty() {
            return Ok(());
        }
        let meta = SessionMeta {
            id: self.id.clone(),
            cwd: self.cwd.clone(),
            created_ms: self.created_ms,
            modified_ms: crate::message::now_millis(),
            first_prompt: self.first_prompt.clone(),
        };
        crate::session_catalog::register_session(&self.home, &self.dir, &meta)
    }
}

#[cfg(test)]
mod fork_tests {
    use super::*;

    fn temp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "feather-fork-{tag}-{}-{}",
            std::process::id(),
            crate::message::now_millis()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn fork_copies_history_and_gets_its_own_identity() {
        let home = temp("home");
        let cwd = temp("cwd");
        let mut s = Session::new(&home, &cwd).unwrap();
        s.push(Message::user_text("before fork")).unwrap();
        s.commit_turn().unwrap();

        let mut f = s.fork().unwrap();
        assert_ne!(f.id(), s.id());
        assert_eq!(f.dir().parent(), s.dir().parent());
        // the fork resumes the same context…
        assert_eq!(f.messages().len(), 1);
        assert_eq!(f.messages()[0].text(), "before fork");
        // …and continues independently
        f.push(Message::user_text("after fork")).unwrap();
        f.commit_turn().unwrap();
        assert_eq!(f.turn(), 2);
        // original untouched
        assert_eq!(s.turn(), 1);
        assert_eq!(s.messages().len(), 1);
        let _ = std::fs::remove_dir_all(&home);
        let _ = std::fs::remove_dir_all(&cwd);
    }
}

pub fn add_usage(mut a: Usage, b: Usage) -> Usage {
    a.input += b.input;
    a.output += b.output;
    a.cache_read += b.cache_read;
    a.cache_write += b.cache_write;
    a
}

#[cfg(test)]
mod tests {
    #[test]
    fn shared_requests_and_cached_metrics_track_push_compaction_and_retention() {
        let mut session = Session::memory(Path::new("fixture"), "metrics");
        let check = |s: &Session| {
            assert_eq!(
                s.context_tokens(),
                crate::tokenizer::estimate_context_tokens(s.messages())
            );
            assert_eq!(s.history_hash(), SnapshotStore::history_hash(s.messages()));
            assert_eq!(s.message_hashes().len(), s.messages().len());
            for (index, message) in s.messages().iter().enumerate() {
                assert_eq!(
                    s.message_hashes()[index],
                    crate::message::fnv1a(&serde_json::to_vec(message).unwrap())
                );
                assert_eq!(
                    s.prefix_tokens()[index + 1],
                    crate::tokenizer::estimate_context_tokens(&s.messages()[..=index])
                );
            }
        };
        check(&session);
        for n in 0..50 {
            session
                .push(Message::user_text(format!("{n} 羽毛 repeat")))
                .unwrap();
            check(&session);
            let shared = session.shared_messages();
            assert!(std::ptr::eq(
                session.messages().as_ptr(),
                shared.as_slice().as_ptr()
            ));
            drop(shared);
        }
        let retained = session.shared_messages();
        let old_len = retained.len();
        session.push(Message::user_text("new")).unwrap();
        assert_eq!(retained.len(), old_len);
        check(&session);
        session.set_messages_for_compaction(vec![Message::user_text("summary")]);
        check(&session);
        assert_eq!(retained.len(), old_len);
    }
    #[test]
    fn checkpoint_rejection_is_a_context_diagnostic_not_a_session_abort() {
        let h = home("checkpoint-diagnostic");
        let root = h.join("workspace");
        std::fs::create_dir_all(&root).unwrap();
        let mut session = Session::new(&h, &root).unwrap();
        session.push(Message::user_text("normal prompt")).unwrap();
        let call = crate::message::ToolCall {
            id: "blocked-path".into(),
            name: "write".into(),
            arguments: serde_json::json!({"path":"../outside.txt"}),
        };
        session
            .push(Message::assistant(
                vec![crate::message::ContentBlock::ToolCall(call)],
                "synthetic",
                crate::message::Api::Faux,
                Usage::default(),
                crate::message::StopReason::ToolUse,
            ))
            .unwrap();
        session
            .push(Message::tool_result(
                "blocked-path",
                "write",
                "[ILLEGAL_COMMAND] outside path not executed",
                true,
            ))
            .unwrap();
        assert_eq!(session.commit_turn().unwrap(), 1);
        assert!(session
            .messages()
            .last()
            .unwrap()
            .text()
            .contains("<feather-runtime-diagnostic>"));
        assert!(session
            .messages()
            .last()
            .unwrap()
            .text()
            .contains("ILLEGAL_COMMAND"));
        let dir = session.dir().to_path_buf();
        drop(session);
        let reopened = Session::open(&dir, &h).unwrap();
        assert!(reopened
            .messages()
            .last()
            .unwrap()
            .text()
            .contains("ILLEGAL_COMMAND"));
        assert!(!h.join("outside.txt").exists());
        drop(reopened);
        std::fs::remove_dir_all(h).unwrap();
    }
    use super::*;
    use crate::message::{ContentBlock, StopReason};

    fn home(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "feather-sess-{tag}-{}-{}",
            std::process::id(),
            crate::message::now_millis()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn push_commit_rewind_roundtrip() {
        let h = home("pcr");
        let mut s = Session::new(&h, Path::new("/tmp")).unwrap();
        s.push(Message::user_text("one")).unwrap();
        s.push(Message::assistant(
            vec![ContentBlock::Text { text: "r1".into() }],
            "m",
            crate::message::Api::Faux,
            Usage::default(),
            StopReason::Stop,
        ))
        .unwrap();
        assert_eq!(s.commit_turn().unwrap(), 1);
        s.push(Message::user_text("two")).unwrap();
        s.push(Message::assistant(
            vec![ContentBlock::Text { text: "r2".into() }],
            "m",
            crate::message::Api::Faux,
            Usage::default(),
            StopReason::Stop,
        ))
        .unwrap();
        assert_eq!(s.commit_turn().unwrap(), 2);
        assert_eq!(s.messages().len(), 4);
        // rewind to turn 1
        s.rewind_to(1).unwrap();
        assert_eq!(s.messages().len(), 2);
        assert_eq!(s.messages()[0].text(), "one");
        // raw redb audit still holds everything
        let raw = s.raw_messages().unwrap();
        assert_eq!(raw.len(), 4);
        assert_eq!(raw[2].text(), "two");
        let _ = std::fs::remove_dir_all(&h);
    }

    #[test]
    fn reopen_resumes_from_latest_snapshot() {
        let h = home("ro");
        let mut s = Session::new(&h, Path::new("/tmp")).unwrap();
        s.push(Message::user_text("persist me")).unwrap();
        s.commit_turn().unwrap();
        // uncommitted tail (simulated crash)
        s.push(Message::user_text("lost turn")).unwrap();
        let dir = s.dir().to_path_buf();
        let id = s.id().to_string();
        drop(s);
        let home2 = h.clone();
        let s2 = Session::open(&dir, &home2).unwrap();
        assert_eq!(s2.id(), id);
        assert_eq!(s2.messages().len(), 1);
        assert_eq!(s2.messages()[0].text(), "persist me");
        // the raw audit keeps the uncommitted tail even though context drops it
        assert_eq!(s2.raw_messages().unwrap().len(), 2);
        let _ = std::fs::remove_dir_all(&h);
    }

    #[test]
    fn reopen_after_rewind_restores_current_not_stale_future() {
        let h = home("rr");
        let mut s = Session::new(&h, Path::new("/tmp")).unwrap();
        s.push(Message::user_text("turn one")).unwrap();
        s.commit_turn().unwrap();
        s.push(Message::user_text("turn two")).unwrap();
        s.commit_turn().unwrap();
        s.push(Message::user_text("turn three")).unwrap();
        s.commit_turn().unwrap();
        assert_eq!(s.turn(), 3);

        // rewind and *do not prompt again* — the durable pointer must win
        s.rewind_to(1).unwrap();
        assert_eq!(s.messages().len(), 1);
        let dir = s.dir().to_path_buf();
        drop(s);

        let reopened = Session::open(&dir, &h).unwrap();
        assert_eq!(reopened.turn(), 1);
        assert_eq!(reopened.messages().len(), 1);
        assert_eq!(reopened.messages()[0].text(), "turn one");
        // forward history is still available for an explicit forward rewind
        assert_eq!(reopened.latest_snapshot_turn().unwrap(), 3);
        let _ = std::fs::remove_dir_all(&h);
    }

    #[test]
    fn fork_after_rewind_keeps_current_pointer() {
        let h = home("frk");
        let mut s = Session::new(&h, Path::new("/tmp")).unwrap();
        s.push(Message::user_text("one")).unwrap();
        s.commit_turn().unwrap();
        s.push(Message::user_text("two")).unwrap();
        s.commit_turn().unwrap();
        s.rewind_to(1).unwrap();

        let f = s.fork().unwrap();
        assert_eq!(f.turn(), 1);
        assert_eq!(f.messages().len(), 1);
        assert_eq!(f.messages()[0].text(), "one");
        let fdir = f.dir().to_path_buf();
        drop(f);
        let reopened = Session::open(&fdir, &h).unwrap();
        assert_eq!(reopened.turn(), 1);
        assert_eq!(reopened.messages()[0].text(), "one");
        let _ = std::fs::remove_dir_all(&h);
    }

    #[test]
    fn session_file_points_at_redb_not_jsonl() {
        let h = home("file");
        let s = Session::new(&h, Path::new("/tmp")).unwrap();
        let file = s.session_file().unwrap().to_path_buf();
        assert!(file.ends_with("session.redb"));
        assert!(!file.to_string_lossy().ends_with(".jsonl"));
        assert!(file.exists());
        assert!(!s.dir().join("session.jsonl").exists());
        let _ = std::fs::remove_dir_all(&h);
    }
}
