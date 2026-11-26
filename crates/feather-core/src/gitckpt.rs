//! Session-scoped working-directory checkpoints using git object storage.
//!
//! Safety invariants:
//! - never discover/use an ancestor repository: only `<session cwd>/.git` is
//!   considered, otherwise Feather initializes a repo at exactly the cwd;
//! - pre-existing repos are checkpointed with a temporary index and `commit-tree`,
//!   so HEAD, branch, and the user's index are never changed;
//! - restore only touches files recorded by this session's checkpoints.

use crate::process::git_command;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write as _;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct TurnCommit {
    turn: u64,
    hash: String,
    created_repo: bool,
    files: Vec<String>,
}

pub struct GitCheckpoints {
    start: PathBuf,
}

#[derive(Debug, Clone)]
pub struct CheckpointReport {
    pub hash: Option<String>,
    pub diagnostics: Vec<String>,
}

impl GitCheckpoints {
    pub fn open(cwd: &Path) -> Self {
        Self {
            start: cwd.to_path_buf(),
        }
    }

    fn map_path(session_dir: &Path) -> PathBuf {
        session_dir.join("turn_commits.json")
    }
    /// The turn→commit map lives in its own database, not in
    /// `checkpoints.redb`.
    ///
    /// Both used to share one file because they always ran on one thread, where
    /// the `Records` weak cache guaranteed a single handle. Checkpoints now run
    /// on a worker thread, so the worker would open the same redb the main
    /// thread already holds and redb refuses (`Database already open`). Two
    /// files, two locks, no interaction.
    fn map_db(session_dir: &Path) -> PathBuf {
        session_dir.join("git-turn-commits.redb")
    }

    fn load_map(session_dir: &Path) -> std::io::Result<BTreeMap<u64, TurnCommit>> {
        let store = crate::records::Records::open(&Self::map_db(session_dir))
            .map_err(std::io::Error::other)?;
        if let Some(map) = store.get("turns").map_err(std::io::Error::other)? {
            return Ok(map);
        }
        // Anything older is migrated by `precheck` on the calling thread. It
        // cannot happen here: the worker must never open
        // `checkpoints.redb`, which the main thread already holds.
        let legacy = Self::map_path(session_dir);
        if legacy.exists() {
            let map: BTreeMap<u64, TurnCommit> =
                serde_json::from_slice(&std::fs::read(&legacy)?).map_err(std::io::Error::other)?;
            store.put("turns", &map).map_err(std::io::Error::other)?;
            let verified: BTreeMap<u64, TurnCommit> = store
                .get("turns")
                .map_err(std::io::Error::other)?
                .ok_or_else(|| std::io::Error::other("checkpoint migration verification failed"))?;
            if serde_json::to_vec(&verified)? != serde_json::to_vec(&map)? {
                return Err(std::io::Error::other(
                    "checkpoint migration verification mismatch",
                ));
            }
            std::fs::rename(&legacy, legacy.with_extension("json.legacy"))?;
            return Ok(map);
        }
        Ok(BTreeMap::new())
    }
    /// Best-effort read of the pre-split layout, where the turn map shared
    /// `checkpoints.redb` with file-snapshot state.
    ///
    /// Returns `None` when the file is absent, unreadable, or currently locked
    /// by another handle — all of which simply mean "nothing to migrate".
    fn migrate_from_shared_db(session_dir: &Path) -> Option<BTreeMap<u64, TurnCommit>> {
        let shared = session_dir.join("checkpoints.redb");
        if !shared.is_file() {
            return None;
        }
        let store = crate::records::Records::open(&shared).ok()?;
        let map: BTreeMap<u64, TurnCommit> = store.get("turns").ok()??;
        (!map.is_empty()).then_some(map)
    }

    fn save_map(session_dir: &Path, map: &BTreeMap<u64, TurnCommit>) -> std::io::Result<()> {
        crate::records::Records::open(&Self::map_db(session_dir))
            .and_then(|s| s.put("turns", map))
            .map_err(std::io::Error::other)
    }

    pub fn ensure_repo(&self) -> std::io::Result<(PathBuf, bool)> {
        let root = crate::workspace_path::normalize_input(&self.start);
        std::fs::create_dir_all(&root)?;
        let root = std::fs::canonicalize(root)?;
        if root.join(".git").exists() {
            return Ok((root, false));
        }
        let out = git_command().arg("init").current_dir(&root).output()?;
        if !out.status.success() {
            return Err(std::io::Error::other(format!(
                "git init failed: {}",
                String::from_utf8_lossy(&out.stderr)
            )));
        }
        ensure_gitignore_entries(&root, &[".feather/", ".pi-subagents/", ".pi/"]);
        Ok((root, true))
    }

    fn run(repo: &Path, args: &[&str]) -> std::io::Result<std::process::Output> {
        git_command().args(args).current_dir(repo).output()
    }

    fn check(repo: &Path, args: &[&str]) -> std::io::Result<std::process::Output> {
        let out = Self::run(repo, args)?;
        if out.status.success() {
            Ok(out)
        } else {
            Err(std::io::Error::other(format!(
                "git {:?} failed: {}",
                args,
                String::from_utf8_lossy(&out.stderr)
            )))
        }
    }

    /// Normalize a touched path to a repo-relative POSIX path, keeping only
    /// paths that exist and cannot escape the repo. Absolute paths that point
    /// inside the repo are accepted so tools that hand back absolute paths are
    /// still checkpointed.
    fn clean_files(repo: &Path, files: &[String]) -> (Vec<String>, Vec<String>) {
        let mut out = BTreeSet::new();
        let mut diagnostics = Vec::new();
        for f in files {
            let input = crate::workspace_path::normalize_input(Path::new(f));
            let p = crate::workspace_path::comparison_key(&input);
            let base = crate::workspace_path::comparison_key(repo);
            let rel_path: PathBuf = if p.is_absolute() {
                match p.strip_prefix(&base) {
                    Ok(r) => r.to_path_buf(),
                    Err(_) => {
                        diagnostics.push(
                            "[ILLEGAL_COMMAND] checkpoint skipped path outside workspace"
                                .to_string(),
                        );
                        continue;
                    }
                }
            } else {
                p.to_path_buf()
            };
            let rel = rel_path.to_string_lossy().replace('\\', "/");
            if rel.is_empty()
                || rel == "."
                || rel.split('/').any(|c| c == "..")
                || rel == ".git"
                || rel.starts_with(".git/")
            {
                diagnostics.push(format!(
                    "[ILLEGAL_COMMAND] checkpoint skipped unsafe path: {rel}"
                ));
                continue;
            }
            let full = repo.join(&rel);
            let full = crate::workspace_path::normalize_input(&full);
            match std::fs::symlink_metadata(&full) {
                Ok(meta) if meta.file_type().is_symlink() || !meta.is_file() => {}
                Ok(_) => {
                    // A regular file under a symlinked directory can still
                    // escape the workspace; validate the canonical parent chain.
                    if let (Ok(real), Ok(base)) = (full.canonicalize(), repo.canonicalize()) {
                        if !real.starts_with(&base) {
                            diagnostics.push("[ILLEGAL_COMMAND] checkpoint skipped symlink path outside workspace".into());
                            continue;
                        }
                    }
                    out.insert(rel);
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => {
                    diagnostics.push(format!("checkpoint skipped unreadable path {rel}: {e}"))
                }
            }
        }
        (out.into_iter().collect(), diagnostics)
    }

    /// Which of `files` git would ignore, in one process.
    ///
    /// The per-file form (`check-ignore -q -- f`) cost one process per file,
    /// which dominated the checkpoint: a 12-file turn spawned 48 of them. With
    /// `--stdin -z` git reports only the ignored paths, in the order given, so
    /// one spawn answers the whole turn.
    ///
    /// `-z` and a NUL-separated stdin are both required: without them a path
    /// containing a newline is parsed as two paths, and a path that is ignored
    /// could be misattributed to a different entry.
    fn ignored_paths(repo: &Path, files: &[String]) -> BTreeSet<String> {
        if files.is_empty() {
            return BTreeSet::new();
        }
        let mut input = Vec::new();
        for f in files {
            input.extend_from_slice(f.as_bytes());
            input.push(0);
        }
        let mut child = match git_command()
            .args(["check-ignore", "--stdin", "-z"])
            .current_dir(repo)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            Ok(child) => child,
            // A spawn failure is not a reason to drop a checkpoint: treat
            // nothing as ignored and let `git add` do the filtering it already
            // does. `add` refuses ignored paths, which is the same outcome.
            Err(_) => return BTreeSet::new(),
        };
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(&input);
            // Dropping closes the pipe so git can finish.
        }
        match child.wait_with_output() {
            Ok(out) => out
                .stdout
                .split(|b| *b == 0)
                .filter(|chunk| !chunk.is_empty())
                .map(|chunk| String::from_utf8_lossy(chunk).into_owned())
                .collect(),
            Err(_) => BTreeSet::new(),
        }
    }

    /// Stage every accepted file in ONE `git add`.
    ///
    /// `git add -- a b c` takes the whole list. Files git still refuses (a
    /// nested repo, a broken submodule) do not fail the batch in a useful way,
    /// so on error we fall back to per-file adds to keep the precise diagnostic
    /// naming the offending path — the slow path only runs when something is
    /// actually wrong.
    fn add_all(
        repo: &Path,
        index: &Path,
        files: &[String],
        diagnostics: &mut Vec<String>,
    ) -> Vec<String> {
        if files.is_empty() {
            return Vec::new();
        }
        let mut cmd = git_command();
        cmd.arg("add").arg("--");
        cmd.args(files);
        let out = cmd.current_dir(repo).env("GIT_INDEX_FILE", index).output();
        if out.as_ref().is_ok_and(|o| o.status.success()) {
            return files.to_vec();
        }
        // One bad path must not cost the whole checkpoint, so retry singly and
        // report exactly which paths were refused.
        let mut added = Vec::new();
        for f in files {
            let out = git_command()
                .args(["add", "--", f])
                .current_dir(repo)
                .env("GIT_INDEX_FILE", index)
                .output();
            match out {
                Ok(o) if o.status.success() => added.push(f.clone()),
                Ok(o) => diagnostics.push(format!(
                    "[ILLEGAL_COMMAND] checkpoint skipped path {f}: {}",
                    Self::sanitize_git_stderr(&o.stderr, repo)
                )),
                Err(e) => diagnostics.push(format!(
                    "[ILLEGAL_COMMAND] checkpoint skipped path {f}: {e}"
                )),
            }
        }
        added
    }

    fn sanitize_git_stderr(stderr: &[u8], repo: &Path) -> String {
        let mut text = String::from_utf8_lossy(stderr).trim().to_string();
        let repo_text = repo.display().to_string();
        if !repo_text.is_empty() {
            text = text.replace(&repo_text, "<workspace>");
            text = text.replace(&repo_text.replace('\\', "/"), "<workspace>");
        }
        text
    }

    /// Create a session checkpoint. Always records a commit — even for an
    /// empty file set (the empty tree is the baseline marker) — so a later
    /// rewind can remove files this session added after a no-op turn.
    ///
    /// Never moves the user's HEAD or index: commits are built through a
    /// temporary `GIT_INDEX_FILE` and `commit-tree`, leaving the user's refs
    /// and staging area untouched. The checkpoint commit is retained by a
    /// per-session ref so Git garbage collection cannot erase it.
    /// A repo Feather creates itself is restored
    /// exactly; a pre-existing repo only ever sees managed file contents.
    /// Commit a turn's checkpoint, keeping every checkpoint this session has made.
    ///
    /// No window: the caller chose to keep everything. Production uses
    /// [`Self::commit_turn_windowed`], which knows the rewind horizon.
    pub fn commit_turn(
        &self,
        turn: u64,
        session_dir: &Path,
        files: &[String],
    ) -> std::io::Result<Option<String>> {
        Ok(self.commit_turn_report(turn, 0, session_dir, files)?.hash)
    }

    /// Commit a turn's checkpoint, dropping refs older than `keep_from_turn`.
    ///
    /// The floor comes from the session, which derives it from its instruction list:
    /// a rewind window is measured in instructions and one instruction spans dozens
    /// of snapshot turns, so this function cannot compute it from `turn`.
    pub fn commit_turn_windowed(
        &self,
        turn: u64,
        keep_from_turn: u64,
        session_dir: &Path,
        files: &[String],
    ) -> std::io::Result<Option<String>> {
        Ok(self
            .commit_turn_report(turn, keep_from_turn, session_dir, files)?
            .hash)
    }

    /// Validate the touched paths *without* spawning git.
    ///
    /// `clean_files` is pure filesystem work: it normalizes paths, rejects
    /// escapes and non-files, and produces the diagnostics the model needs.
    /// Running it on the caller's thread keeps those diagnostics on the turn
    /// they belong to, which matters: a model told "that path was skipped" on
    /// the same turn stops retrying it, while a diagnostic delivered a turn
    /// late has already cost another rejected call.
    ///
    /// Returns the accepted repo-relative paths plus diagnostics, or `None`
    /// when the repo cannot be established (in which case the caller leaves
    /// the checkpoint to the worker's error report).
    pub fn precheck(
        &self,
        session_dir: &Path,
        files: &[String],
    ) -> Option<(Vec<String>, Vec<String>)> {
        // One-time migration of the pre-split layout, on the caller's thread
        // (the worker must never open `checkpoints.redb`).
        //
        // Guarded by the new database's existence: once it is there the
        // migration is over, and re-reading the old file every turn would put a
        // redb open back on the turn boundary — which is the cost this whole
        // change exists to remove.
        if !Self::map_db(session_dir).is_file() {
            if let Some(map) = Self::migrate_from_shared_db(session_dir) {
                if let Ok(store) = crate::records::Records::open(&Self::map_db(session_dir)) {
                    let _ = store.put("turns", &map);
                }
            }
        }
        let root = crate::workspace_path::normalize_input(&self.start);
        // `ensure_repo` would create `.git`; probing only needs the directory.
        let repo = std::fs::canonicalize(&root).ok()?;
        Some(Self::clean_files(&repo, files))
    }

    /// Do the git half of a checkpoint for already-validated `files`.
    ///
    /// This is the expensive part (spawns), which is why it runs on the
    /// checkpoint worker rather than at the turn boundary.
    pub fn commit_prepared(
        &self,
        turn: u64,
        keep_from_turn: u64,
        session_dir: &Path,
        files: Vec<String>,
    ) -> std::io::Result<CheckpointReport> {
        self.commit_turn_report(turn, keep_from_turn, session_dir, &files)
    }

    pub fn commit_turn_report(
        &self,
        turn: u64,
        keep_from_turn: u64,
        session_dir: &Path,
        files: &[String],
    ) -> std::io::Result<CheckpointReport> {
        let (repo, created) = self.ensure_repo()?;
        let (files, mut diagnostics) = Self::clean_files(&repo, files);

        let session_root = crate::workspace_path::normalize_input(session_dir);
        std::fs::create_dir_all(&session_root)?;
        let session_root = std::fs::canonicalize(session_root)?;
        let index = crate::workspace_path::comparison_key(&session_root)
            .join(format!("git-index-{turn}.tmp"));

        let _ = std::fs::remove_file(&index);
        // Two spawns for the whole turn instead of two per file: one
        // `check-ignore` for every path, then one `add` for the survivors.
        let ignored = Self::ignored_paths(&repo, &files);
        let staged: Vec<String> = files
            .iter()
            .filter(|f| !ignored.contains(*f))
            .cloned()
            .collect();
        let added = Self::add_all(&repo, &index, &staged, &mut diagnostics);
        let out = git_command()
            .arg("write-tree")
            .current_dir(&repo)
            .env("GIT_INDEX_FILE", &index)
            .output()?;
        let _ = std::fs::remove_file(&index);
        if !out.status.success() {
            return Err(std::io::Error::other(format!(
                "git write-tree failed: {}",
                Self::sanitize_git_stderr(&out.stderr, &repo)
            )));
        }
        let tree = String::from_utf8_lossy(&out.stdout).trim().to_string();
        let out = git_command()
            .args([
                "-c",
                "user.name=feather",
                "-c",
                "user.email=feather@local",
                "commit-tree",
                &tree,
                "-m",
                &format!("feather: turn {turn}"),
            ])
            .current_dir(&repo)
            .output()?;
        if !out.status.success() {
            return Err(std::io::Error::other(format!(
                "git commit-tree failed: {}",
                Self::sanitize_git_stderr(&out.stderr, &repo)
            )));
        }
        let hash = String::from_utf8_lossy(&out.stdout).trim().to_string();
        let ref_name = format!(
            "refs/feather/{}/turn-{turn}",
            session_dir
                .file_name()
                .map(|n| n.to_string_lossy())
                .unwrap_or_default()
        );
        let _ = Self::check(&repo, &["update-ref", &ref_name, &hash]);
        let mut map = Self::load_map(session_dir)?;
        for stale in map.keys().copied().filter(|t| *t > turn) {
            let ref_name = format!(
                "refs/feather/{}/turn-{stale}",
                session_dir
                    .file_name()
                    .map(|n| n.to_string_lossy())
                    .unwrap_or_default()
            );
            let _ = Self::run(&repo, &["update-ref", "-d", &ref_name]);
        }
        map.retain(|t, _| *t <= turn);
        map.insert(
            turn,
            TurnCommit {
                turn,
                hash: hash.clone(),
                created_repo: created,
                files: added,
            },
        );
        // Rewind is bounded, so checkpoints before the floor are unreachable and only
        // accumulate git objects. The floor arrives from the session, which computes
        // it from its instruction list — a turn count here would be the wrong unit
        // (one instruction spans dozens of turns). Only the refs are deleted: the
        // objects are left to git's own gc, since deleting a ref frees nothing.
        let expired: Vec<u64> = map
            .keys()
            .copied()
            .filter(|t| *t < keep_from_turn)
            .collect();
        for stale in expired {
            let ref_name = format!(
                "refs/feather/{}/turn-{stale}",
                session_dir
                    .file_name()
                    .map(|n| n.to_string_lossy())
                    .unwrap_or_default()
            );
            let _ = Self::run(&repo, &["update-ref", "-d", &ref_name]);
            map.remove(&stale);
        }
        Self::save_map(session_dir, &map)?;
        Ok(CheckpointReport {
            hash: Some(hash),
            diagnostics,
        })
    }

    /// Restore the working tree to `turn`, touching **only files this session
    /// recorded as managed**. This is deliberately selective even for a repo
    /// feather created itself: creating `.git` in the user's directory does not
    /// make unrelated files feather's, so `reset --hard` + `clean` are never
    /// used and a user file the agent never touched is never removed.
    ///
    /// Before a managed file is overwritten or removed its current bytes are
    /// copied to `<session_dir>/restore-backup/turn-<turn>/<path>`, so a user
    /// edit that happened to land inside a managed file is recoverable. A
    /// backup write failure aborts the restore rather than destroying data.
    pub fn restore(&self, turn: u64, session_dir: &Path) -> std::io::Result<()> {
        self.restore_excluding(turn, session_dir, &[])
    }

    pub fn restore_excluding(
        &self,
        turn: u64,
        session_dir: &Path,
        excluded: &[PathBuf],
    ) -> std::io::Result<()> {
        let map = Self::load_map(session_dir)?;
        let rec = map
            .get(&turn)
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!("no git checkpoint for turn {turn}"),
                )
            })?
            .clone();
        let (repo, _) = self.ensure_repo()?;

        let mut ever = BTreeSet::new();
        let mut known_at_or_before_target = BTreeSet::new();
        for r in map.values() {
            ever.extend(r.files.iter().cloned());
            if r.turn <= turn {
                known_at_or_before_target.extend(r.files.iter().cloned());
            }
        }
        let target: BTreeSet<String> = rec.files.iter().cloned().collect();
        let backup_root = session_dir
            .join("restore-backup")
            .join(format!("turn-{turn}"));

        // A file is removable only when an earlier checkpoint proves it was
        // managed at or before the target. A later-only file may be unrelated
        // user work (or an omitted path after compaction) and must survive.
        for f in &ever {
            let path = crate::workspace_path::normalize_input(&repo.join(f));
            if excluded.iter().any(|skip| {
                crate::workspace_path::comparison_key(skip)
                    == crate::workspace_path::comparison_key(&path)
            }) {
                continue;
            }
            if target.contains(f) {
                if rec.hash.is_empty() {
                    continue;
                }
                let out = Self::check(&repo, &["show", &format!("{}:{}", rec.hash, f)])?;
                let current = std::fs::read(&path).ok();
                if current.as_deref() == Some(out.stdout.as_slice()) {
                    continue; // already at the checkpointed bytes
                }
                if let Some(bytes) = current.as_deref() {
                    Self::backup(&backup_root, f, bytes)?;
                }
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(&path, out.stdout)?;
            } else if !known_at_or_before_target.contains(f) && path.exists() {
                let bytes = std::fs::read(&path)?;
                Self::backup(&backup_root, f, &bytes)?;
                std::fs::remove_file(&path)?;
            }
        }
        Ok(())
    }

    /// Copy the current bytes of a managed file into the restore backup tree.
    fn backup(root: &Path, file: &str, bytes: &[u8]) -> std::io::Result<()> {
        let dest = root.join(file);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(dest, bytes)
    }

    pub fn has_checkpoint(&self, turn: u64, session_dir: &Path) -> bool {
        Self::load_map(session_dir).is_ok_and(|map| map.contains_key(&turn))
    }
}

pub fn ensure_gitignore_entries(root: &Path, entries: &[&str]) {
    let gi = root.join(".gitignore");
    let existing = std::fs::read_to_string(&gi).unwrap_or_default();
    let mut next = existing.clone();
    for e in entries {
        if !existing.split('\n').any(|l| l.trim() == *e) {
            if !next.is_empty() && !next.ends_with('\n') {
                next.push('\n');
            }
            next.push_str(e);
            next.push('\n');
        }
    }
    if next != existing {
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&gi)
        {
            let _ = f.write_all(next.as_bytes());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn git(repo: &Path, args: &[&str]) -> String {
        let out = git_command()
            .args(["-c", "user.name=feather", "-c", "user.email=feather@local"])
            .args(args)
            .current_dir(repo)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).to_string()
    }
    fn temp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "feather-gitckpt-{tag}-{}-{}",
            std::process::id(),
            crate::message::now_millis()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn does_not_capture_parent_repo_and_restores_managed_tree() {
        let parent = temp("parent");
        git(&parent, &["init"]);
        std::fs::write(parent.join("parent.txt"), "p").unwrap();
        git(&parent, &["add", "parent.txt"]);
        git(&parent, &["commit", "-m", "p"]);
        let child = parent.join("child");
        std::fs::create_dir_all(&child).unwrap();
        let session = child.join(".feather/sessions/s1");
        std::fs::create_dir_all(&session).unwrap();
        let ck = GitCheckpoints::open(&child);
        std::fs::write(child.join("a.txt"), "v1").unwrap();
        ck.commit_turn(1, &session, &["a.txt".into()])
            .unwrap()
            .unwrap();
        assert!(child.join(".git").exists());
        assert_eq!(
            git(&parent, &["rev-parse", "--show-toplevel"]).trim(),
            parent.display().to_string().replace('\\', "/")
        );
        std::fs::write(child.join("a.txt"), "v2").unwrap();
        std::fs::write(child.join("b.txt"), "b").unwrap();
        ck.commit_turn(2, &session, &["a.txt".into(), "b.txt".into()])
            .unwrap()
            .unwrap();
        ck.restore(1, &session).unwrap();
        assert_eq!(std::fs::read_to_string(child.join("a.txt")).unwrap(), "v1");
        assert!(!child.join("b.txt").exists());
        let _ = std::fs::remove_dir_all(parent);
    }

    #[test]
    fn existing_repo_preserves_head_index_and_unrelated_changes() {
        let dir = temp("shared");
        git(&dir, &["init"]);
        std::fs::write(dir.join("tracked.txt"), "base").unwrap();
        std::fs::write(dir.join("staged.txt"), "s0").unwrap();
        git(&dir, &["add", "tracked.txt", "staged.txt"]);
        git(&dir, &["commit", "-m", "base"]);
        let head = git(&dir, &["rev-parse", "HEAD"]);
        std::fs::write(dir.join("staged.txt"), "s1").unwrap();
        git(&dir, &["add", "staged.txt"]);
        std::fs::write(dir.join("tracked.txt"), "dirty").unwrap();
        let session = dir.join(".feather/sessions/s1");
        std::fs::create_dir_all(&session).unwrap();
        let ck = GitCheckpoints::open(&dir);
        std::fs::write(dir.join("agent.txt"), "v1").unwrap();
        ck.commit_turn(1, &session, &["agent.txt".into()])
            .unwrap()
            .unwrap();
        std::fs::write(dir.join("agent.txt"), "v2").unwrap();
        std::fs::write(dir.join("later.txt"), "later").unwrap();
        ck.commit_turn(2, &session, &["agent.txt".into(), "later.txt".into()])
            .unwrap()
            .unwrap();
        ck.restore(1, &session).unwrap();
        assert_eq!(git(&dir, &["rev-parse", "HEAD"]), head);
        assert_eq!(
            std::fs::read_to_string(dir.join("tracked.txt")).unwrap(),
            "dirty"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("agent.txt")).unwrap(),
            "v1"
        );
        assert!(!dir.join("later.txt").exists());
        assert!(git(&dir, &["diff", "--cached", "--name-only"]).contains("staged.txt"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn gitignore_entries_are_idempotent() {
        let dir = temp("gitignore");
        ensure_gitignore_entries(&dir, &[".pi-subagents/", ".pi/"]);
        ensure_gitignore_entries(&dir, &[".pi-subagents/"]);
        let txt = std::fs::read_to_string(dir.join(".gitignore")).unwrap();
        assert_eq!(txt.matches(".pi-subagents/").count(), 1);
        assert!(txt.contains(".pi/"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn no_file_turn_is_an_exact_baseline_for_a_created_repo() {
        let dir = temp("managed-baseline");
        let session = dir.join(".feather/sessions/s1");
        std::fs::create_dir_all(&session).unwrap();
        let ck = GitCheckpoints::open(&dir);
        // turn 1 touches nothing: the empty tree is still recorded
        assert!(ck.commit_turn(1, &session, &[]).unwrap().is_some());
        std::fs::write(dir.join("agent.txt"), "v1").unwrap();
        ck.commit_turn(2, &session, &["agent.txt".into()]).unwrap();
        // rewind to the baseline removes the later managed file
        ck.restore(1, &session).unwrap();
        assert!(!dir.join("agent.txt").exists());
        // two consecutive restores must not destroy session machinery
        ck.restore(1, &session).unwrap();
        assert!(
            dir.join(".feather/sessions/s1").exists(),
            "session dir preserved"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn empty_baseline_preserves_unrelated_files_in_a_shared_repo() {
        let dir = temp("shared-baseline");
        git(&dir, &["init"]);
        std::fs::write(dir.join("user.txt"), "user").unwrap();
        git(&dir, &["add", "user.txt"]);
        git(&dir, &["commit", "-m", "base"]);
        std::fs::write(dir.join("unrelated.txt"), "keep me").unwrap();
        let session = dir.join(".feather/sessions/s1");
        std::fs::create_dir_all(&session).unwrap();
        let ck = GitCheckpoints::open(&dir);
        ck.commit_turn(1, &session, &[]).unwrap();
        std::fs::write(dir.join("agent.txt"), "v1").unwrap();
        ck.commit_turn(2, &session, &["agent.txt".into()]).unwrap();
        ck.restore(1, &session).unwrap();
        assert!(!dir.join("agent.txt").exists(), "managed file removed");
        assert_eq!(
            std::fs::read_to_string(dir.join("unrelated.txt")).unwrap(),
            "keep me"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("user.txt")).unwrap(),
            "user"
        );
        assert!(dir.join(".feather/sessions/s1").exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn absolute_managed_paths_are_normalized() {
        let dir = temp("abs-paths");
        let session = dir.join(".feather/sessions/s1");
        std::fs::create_dir_all(&session).unwrap();
        let ck = GitCheckpoints::open(&dir);
        let abs = dir.join("a.txt");
        std::fs::write(&abs, "v1").unwrap();
        ck.commit_turn(1, &session, &[abs.display().to_string()])
            .unwrap();
        std::fs::write(&abs, "v2").unwrap();
        ck.restore(1, &session).unwrap();
        assert_eq!(std::fs::read_to_string(&abs).unwrap(), "v1");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn created_repo_restore_keeps_unrelated_user_files() {
        let dir = temp("created-preserve");
        let session = dir.join(".feather/sessions/s1");
        std::fs::create_dir_all(&session).unwrap();
        let ck = GitCheckpoints::open(&dir);
        // a user file that predates feather's `.git` and is never touched
        std::fs::write(dir.join("user-notes.txt"), "mine").unwrap();
        std::fs::write(dir.join("a.txt"), "v1").unwrap();
        ck.commit_turn(1, &session, &["a.txt".into()]).unwrap();
        std::fs::write(dir.join("a.txt"), "v2").unwrap();
        ck.commit_turn(2, &session, &["a.txt".into()]).unwrap();
        ck.restore(1, &session).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), "v1");
        assert_eq!(
            std::fs::read_to_string(dir.join("user-notes.txt")).unwrap(),
            "mine",
            "creating `.git` must not make unrelated user files disposable"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn restore_backs_up_replaced_and_removed_files() {
        let dir = temp("restore-backup");
        let session = dir.join(".feather/sessions/s1");
        std::fs::create_dir_all(&session).unwrap();
        let ck = GitCheckpoints::open(&dir);
        std::fs::write(dir.join("a.txt"), "v1").unwrap();
        ck.commit_turn(1, &session, &["a.txt".into()]).unwrap();
        std::fs::write(dir.join("a.txt"), "v2").unwrap();
        std::fs::write(dir.join("b.txt"), "later").unwrap();
        ck.commit_turn(2, &session, &["a.txt".into(), "b.txt".into()])
            .unwrap();
        // the user edits a managed file after the checkpoint
        std::fs::write(dir.join("a.txt"), "user edited").unwrap();
        ck.restore(1, &session).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), "v1");
        assert!(!dir.join("b.txt").exists());
        let backup = session.join("restore-backup").join("turn-1");
        assert_eq!(
            std::fs::read_to_string(backup.join("a.txt")).unwrap(),
            "user edited"
        );
        assert_eq!(
            std::fs::read_to_string(backup.join("b.txt")).unwrap(),
            "later"
        );
        let _ = std::fs::remove_dir_all(dir);
    }
    #[test]
    fn checkpoint_skips_nonfiles_ignored_and_outside_paths_safely() {
        let dir = temp("skip-paths");
        let outside = temp("outside-path");
        let session = dir.join(".feather/sessions/s1");
        std::fs::create_dir_all(&session).unwrap();
        std::fs::write(dir.join(".gitignore"), "ignored.log\n").unwrap();
        std::fs::write(dir.join("ignored.log"), "ignored").unwrap();
        std::fs::create_dir_all(dir.join("adir")).unwrap();
        std::fs::write(outside.join("secret.txt"), "secret").unwrap();
        let ck = GitCheckpoints::open(&dir);
        let report = ck
            .commit_turn_report(
                1,
                0,
                &session,
                &[
                    "ignored.log".into(),
                    "adir".into(),
                    outside.join("secret.txt").display().to_string(),
                ],
            )
            .unwrap();
        assert!(report.hash.is_some());
        assert!(report.diagnostics.iter().all(|d| !d.contains("secret.txt")));
        assert!(report
            .diagnostics
            .iter()
            .all(|d| !d.contains(&outside.display().to_string())));
        assert!(report
            .diagnostics
            .iter()
            .any(|d| d.contains("outside workspace")));
        assert!(!report.diagnostics.iter().any(|d| d.contains("ignored.log")));
        assert!(!report.diagnostics.iter().any(|d| d.contains("adir")));
        let _ = std::fs::remove_dir_all(dir);
        let _ = std::fs::remove_dir_all(outside);
    }
    /// One `check-ignore` must classify every path. The per-file form cost a
    /// process per file; this asserts the batched answer is the same set.
    #[test]
    fn ignored_paths_classifies_the_whole_list_in_one_pass() {
        let dir = temp("ignored-batch");
        git(&dir, &["init"]);
        std::fs::write(dir.join(".gitignore"), "*.log\nbuild/\n").unwrap();
        std::fs::create_dir_all(dir.join("build")).unwrap();
        for f in ["a.log", "b.log", "keep.txt"] {
            std::fs::write(dir.join(f), "x").unwrap();
        }
        std::fs::write(dir.join("build/out.o"), "x").unwrap();
        let files: Vec<String> = ["a.log", "keep.txt", "b.log", "build/out.o"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let ignored = GitCheckpoints::ignored_paths(&dir, &files);
        assert!(ignored.contains("a.log"));
        assert!(ignored.contains("b.log"));
        assert!(ignored.contains("build/out.o"));
        assert!(!ignored.contains("keep.txt"));
        let _ = std::fs::remove_dir_all(dir);
    }

    /// An empty list must not spawn git at all.
    #[test]
    fn ignored_paths_is_free_when_there_is_nothing_to_check() {
        let dir = temp("ignored-empty");
        let ignored = GitCheckpoints::ignored_paths(&dir, &[]);
        assert!(ignored.is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    /// The batch must stage every path, and the fallback must still name the
    /// single path that git refuses rather than failing the whole checkpoint.
    #[test]
    fn add_all_stages_the_batch_and_reports_only_refused_paths() {
        let dir = temp("add-batch");
        git(&dir, &["init"]);
        for f in ["a.txt", "b.txt", "c.txt"] {
            std::fs::write(dir.join(f), "x").unwrap();
        }
        let index = dir.join("batch-index.tmp");
        let files: Vec<String> = ["a.txt", "b.txt", "c.txt"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let mut diagnostics = Vec::new();
        let added = GitCheckpoints::add_all(&dir, &index, &files, &mut diagnostics);
        assert_eq!(added.len(), 3, "all three staged in one add");
        assert!(diagnostics.is_empty());

        // A path that does not exist is refused; the others still stage.
        let mixed: Vec<String> = ["a.txt", "missing.txt"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let mut diagnostics = Vec::new();
        let added = GitCheckpoints::add_all(&dir, &index, &mixed, &mut diagnostics);
        assert_eq!(added, vec!["a.txt".to_string()]);
        assert_eq!(diagnostics.len(), 1, "exactly the refused path is reported");
        assert!(diagnostics[0].contains("missing.txt"));
        let _ = std::fs::remove_file(&index);
        let _ = std::fs::remove_dir_all(dir);
    }
}
