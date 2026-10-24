//! AnchorStore: per-file anchor maps with shared redb persistence.
//!
//! The store owns one `AnchorMap` per touched file and persists each mapping
//! inside `<home>/anchors.redb`; legacy sidecars are imported when encountered.
//! A restarted session re-adopts anchors only when all line hashes match.
//! All tools use these operations; current content is verified on every edit.

use crate::engine::{
    Anchor, AnchorMap, DiffRow, Direction, EngineError, PersistRecord, MAX_MAP_BYTES,
};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub struct AnchorStore {
    maps: Mutex<HashMap<PathBuf, AnchorMap>>,
    home: PathBuf,
    records: Mutex<Option<feather_core::records::Records>>,
    snapshots: Option<feather_core::file_snapshots::FileSnapshots>,

    /// Most recent touch time per path — tiebreaker when an anchor exists in
    /// several mapped files (anchor-only resolution).
    last_touched: Mutex<HashMap<PathBuf, u64>>,
}

impl AnchorStore {
    pub fn new(home: &Path) -> Self {
        Self {
            maps: Mutex::new(HashMap::new()),
            home: home.to_path_buf(),
            records: Mutex::new(None),
            snapshots: None,

            last_touched: Mutex::new(HashMap::new()),
        }
    }

    fn records(&self) -> Result<feather_core::records::Records, String> {
        let mut records = self.records.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(store) = records.as_ref() {
            return Ok(store.clone());
        }
        let store = feather_core::records::Records::open(&self.home.join("anchors.redb"))?;
        *records = Some(store.clone());
        Ok(store)
    }
    /// Track every anchor edit in the parent session's redb file history.
    pub fn with_snapshots(mut self, session_dir: &Path, _workspace: &Path) -> Self {
        self.snapshots = Some(feather_core::file_snapshots::FileSnapshots::new(
            session_dir,
        ));
        self
    }
    fn before_edit(&self, path: &Path) -> Result<(), EngineError> {
        if let Some(s) = &self.snapshots {
            s.before_edit(path)
                .map_err(|e| EngineError::Io(format!("file snapshot: {e}")))?;
        }

        Ok(())
    }
    fn after_edit(&self, path: &Path) -> Result<(), EngineError> {
        if let Some(s) = &self.snapshots {
            s.after_edit(path)
                .map_err(|e| EngineError::Io(format!("file snapshot: {e}")))?;
        }

        Ok(())
    }
    fn sidecar_path(&self, path: &Path) -> PathBuf {
        let path = &norm_key(path);
        let key = feather_core::message::fnv1a(path.to_string_lossy().as_bytes());
        self.home.join("anchors").join(format!("{key:016x}.json"))
    }

    fn seed_for(path: &Path, content: &str) -> u64 {
        let path = &norm_key(path);
        feather_core::message::fnv1a(
            format!(
                "feather-anchor-v1:{}:{}",
                path.display(),
                feather_core::message::fnv1a(content.as_bytes())
            )
            .as_bytes(),
        )
    }

    /// Ensure the file is mapped and current (re-map when the disk content
    /// diverged from the cached map — external edit detection).
    fn ensure(&self, path: &Path) -> Result<(), EngineError> {
        {
            let maps = self.maps.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(m) = maps.get(path) {
                let disk = std::fs::read_to_string(path).unwrap_or_default();
                if disk == m.to_file_content() {
                    return Ok(());
                }
            }
        }
        let map = self.build_map(path)?;
        let mut maps = self.maps.lock().unwrap_or_else(|e| e.into_inner());
        maps.insert(path.to_path_buf(), map);
        Ok(())
    }

    /// Load + map a file (rejecting binary/oversize), adopting the persisted
    /// anchor table when every line hash matches.
    pub fn build_map(&self, path: &Path) -> Result<AnchorMap, EngineError> {
        let bytes =
            std::fs::read(path).map_err(|e| EngineError::Io(format!("{}: {e}", path.display())))?;
        if bytes.len() as u64 > MAX_MAP_BYTES {
            return Err(EngineError::TooLarge(format!(
                "{} is {} bytes (limit {MAX_MAP_BYTES}); it cannot be mapped",
                path.display(),
                bytes.len()
            )));
        }
        if bytes.contains(&0) {
            return Err(EngineError::Binary(format!(
                "{} looks binary (NUL byte); the hashline tools refuse binary files",
                path.display()
            )));
        }
        if bytes.starts_with(&[0xFF, 0xFE]) || bytes.starts_with(&[0xFE, 0xFF]) {
            return Err(EngineError::Encode(format!(
                "{} is UTF-16; convert to UTF-8 first",
                path.display()
            )));
        }
        let content = String::from_utf8(bytes)
            .map_err(|_| EngineError::Encode(format!("{} is not valid UTF-8", path.display())))?;
        let seed = Self::seed_for(path, &content);
        let mut map = AnchorMap::new(path.to_path_buf(), content, seed)?;
        let store = self.records().map_err(EngineError::Io)?;
        let key = norm_key(path).display().to_string();
        let rec = store.get::<PersistRecord>(&key).map_err(EngineError::Io)?;
        if let Some(rec) = rec {
            if rec.hashes.len() == map.len()
                && rec
                    .hashes
                    .iter()
                    .zip(map.lines.iter())
                    .all(|(h, l)| *h == feather_core::message::fnv1a(l.as_bytes()))
            {
                map.adopt_persist(rec);
            }
        } else {
            let legacy = self.sidecar_path(path);
            if legacy.exists() {
                let rec: PersistRecord = serde_json::from_slice(
                    &std::fs::read(&legacy).map_err(|e| EngineError::Io(e.to_string()))?,
                )
                .map_err(|e| EngineError::Io(e.to_string()))?;
                store.put(&key, &rec).map_err(EngineError::Io)?;
                std::fs::rename(&legacy, legacy.with_extension("json.legacy"))
                    .map_err(|e| EngineError::Io(e.to_string()))?;
                if rec.hashes.len() == map.len()
                    && rec
                        .hashes
                        .iter()
                        .zip(map.lines.iter())
                        .all(|(h, l)| *h == feather_core::message::fnv1a(l.as_bytes()))
                {
                    map.adopt_persist(rec);
                }
            }
        }
        self.persist(&map);
        Ok(map)
    }

    /// Read rows for `path` (auto-map). `offset` is 1-indexed; `None` reads
    /// everything within the guard.
    pub fn read(
        &self,
        path: &Path,
        offset: Option<usize>,
        limit: Option<usize>,
    ) -> Result<Vec<String>, EngineError> {
        self.ensure(path)?;
        self.touch(path);
        let maps = self.maps.lock().unwrap_or_else(|e| e.into_inner());
        let m = maps
            .get(path)
            .ok_or_else(|| EngineError::Io("map vanished".into()))?;
        let start = offset.unwrap_or(1).saturating_sub(1);
        let end = match limit {
            Some(l) => start.saturating_add(l).saturating_sub(1),
            None => m.len().saturating_sub(1),
        };
        Ok(m.serve_rows(start, end))
    }

    pub fn total_lines(&self, path: &Path) -> Result<usize, EngineError> {
        self.ensure(path)?;
        let maps = self.maps.lock().unwrap_or_else(|e| e.into_inner());
        Ok(maps.get(path).map(|m| m.len()).unwrap_or(0))
    }

    pub fn replace(
        &self,
        path: &Path,
        from: Anchor,
        to: Anchor,
        replacement: &str,
    ) -> Result<Vec<DiffRow>, EngineError> {
        self.ensure(path)?;
        self.with_map_mut(path, |m| m.replace(from, to, replacement))
    }

    pub fn replace_within(
        &self,
        path: &Path,
        from: Anchor,
        to: Anchor,
        old: &str,
        new: &str,
    ) -> Result<Vec<DiffRow>, EngineError> {
        self.ensure(path)?;
        self.with_map_mut(path, |m| m.replace_within(from, to, old, new))
    }

    pub fn insert(
        &self,
        path: &Path,
        anchor: Anchor,
        direction: Direction,
        text: &str,
    ) -> Result<Vec<DiffRow>, EngineError> {
        self.ensure(path)?;
        self.with_map_mut(path, |m| m.insert(anchor, direction, text))
    }

    pub fn undo(&self, path: &Path) -> Result<Vec<DiffRow>, EngineError> {
        self.ensure(path)?;
        self.with_map_mut(path, |m| m.undo())
    }

    /// Full rewrite (write tool): replace content, rebuild mapping.
    pub fn write(&self, path: &Path, content: &str) -> Result<Vec<String>, EngineError> {
        if content.as_bytes().contains(&0) {
            return Err(EngineError::Binary("content has NUL bytes".into()));
        }
        if content.len() as u64 > MAX_MAP_BYTES {
            return Err(EngineError::TooLarge("content exceeds map limit".into()));
        }
        self.before_edit(path)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| EngineError::Io(e.to_string()))?;
        }
        std::fs::write(path, content).map_err(|e| EngineError::Io(e.to_string()))?;
        self.after_edit(path)?;
        self.invalidate(path);
        self.ensure(path)?;
        self.read(path, None, None)
    }

    /// Copy a source anchor range after `insert_after` (anchors resolve in
    /// possibly different files). The source lines stay in place.
    pub fn copy(
        &self,
        src_path: &Path,
        source_from: Anchor,
        source_to: Anchor,
        dst_path: &Path,
        insert_after: Anchor,
    ) -> Result<Vec<DiffRow>, EngineError> {
        self.ensure(src_path)?;
        let payload = {
            let mut maps = self.maps.lock().unwrap_or_else(|e| e.into_inner());
            let m = maps
                .get_mut(src_path)
                .ok_or_else(|| EngineError::Io("map vanished".into()))?;
            m.take_range(source_from, source_to)?
        };
        self.ensure(dst_path)?;
        self.with_map_mut(dst_path, |m| m.paste_range(insert_after, &payload, false))
    }

    /// Move a range: removed from source, inserted into destination (same
    /// file allowed; `insert_after` must sit outside the source range then).
    pub fn move_range(
        &self,
        src_path: &Path,
        source_from: Anchor,
        source_to: Anchor,
        dst_path: &Path,
        insert_after: Anchor,
    ) -> Result<(Vec<DiffRow>, Vec<DiffRow>), EngineError> {
        self.ensure(src_path)?;
        self.ensure(dst_path)?;
        if src_path == dst_path {
            let maps = self.maps.lock().unwrap_or_else(|e| e.into_inner());
            let m = maps
                .get(src_path)
                .ok_or_else(|| EngineError::Io("map vanished".into()))?;
            let a = m
                .line_index(source_from)
                .ok_or_else(|| bad_anchor(source_from))?;
            let b = m
                .line_index(source_to)
                .ok_or_else(|| bad_anchor(source_to))?;
            let t = m
                .line_index(insert_after)
                .ok_or_else(|| bad_anchor(insert_after))?;
            if a <= t && t <= b {
                return Err(EngineError::InvalidRange(
                    "insert_after must sit outside the source range when both anchors share a file"
                        .into(),
                ));
            }
            if t == b {
                // removing [a..b] then pasting after the range's own last
                // line restores the original order
                return Err(EngineError::NoChange(
                    "moving a range to where it already sits".into(),
                ));
            }
        }
        let payload = {
            let mut maps = self.maps.lock().unwrap_or_else(|e| e.into_inner());
            let m = maps
                .get_mut(src_path)
                .ok_or_else(|| EngineError::Io("map vanished".into()))?;
            m.take_range(source_from, source_to)?
        };
        let removed = self.with_map_mut(src_path, |m| m.remove_range(source_from, source_to))?;
        let same_file = src_path == dst_path;
        // after removal the destination map must be re-read for same-file
        // moves: the insert_after anchor survives (it is outside the range)
        let added = self.with_map_mut(dst_path, |m| {
            m.paste_range(insert_after, &payload, same_file)
        })?;
        Ok((removed, added))
    }

    fn with_map_mut<T>(
        &self,
        path: &Path,
        f: impl FnOnce(&mut AnchorMap) -> Result<T, EngineError>,
    ) -> Result<T, EngineError> {
        self.before_edit(path)?;
        let mut maps = self.maps.lock().unwrap_or_else(|e| e.into_inner());
        let map = maps
            .get_mut(path)
            .ok_or_else(|| EngineError::Io("map vanished".into()))?;
        let out = f(map)?;
        let rec = map.persist_record();
        drop(maps);
        self.persist_record(&rec);
        self.after_edit(path)?;
        Ok(out)
    }

    fn persist(&self, map: &AnchorMap) {
        self.persist_record(&map.persist_record());
    }

    fn persist_record(&self, rec: &PersistRecord) {
        self.records()
            .and_then(|s| s.put(&norm_key(Path::new(&rec.path)).display().to_string(), rec))
            .expect("persist anchor state");
    }

    /// Forget a file's mapping (write/rewrite).
    pub fn invalidate(&self, path: &Path) {
        let mut maps = self.maps.lock().unwrap_or_else(|e| e.into_inner());
        maps.remove(path);
    }

    // ------------------------------------------------------------------
    // anchor-only resolution: tools address files by anchor alone
    // ------------------------------------------------------------------

    /// All cached files whose map currently contains `anchor`, newest-touched
    /// first.
    pub fn resolve_anchor(&self, anchor: Anchor) -> Vec<PathBuf> {
        let maps = self.maps.lock().unwrap_or_else(|e| e.into_inner());
        let mut out: Vec<PathBuf> = maps
            .iter()
            .filter(|(_, m)| m.line_index(anchor).is_some())
            .map(|(p, _)| p.clone())
            .collect();
        let touched = self.last_touched.lock().unwrap_or_else(|e| e.into_inner());
        out.sort_by_key(|p| std::cmp::Reverse(touched.get(p).copied().unwrap_or(0)));
        out
    }

    fn touch(&self, path: &Path) {
        let mut t = self.last_touched.lock().unwrap_or_else(|e| e.into_inner());
        t.insert(path.to_path_buf(), feather_core::message::now_millis());
    }

    /// Resolve a same-file anchor pair (replace / replace_within / insert).
    fn resolve_same_file(&self, a: Anchor, b: Anchor, op: &str) -> Result<PathBuf, EngineError> {
        let ca = self.resolve_anchor(a);
        let cb = self.resolve_anchor(b);
        let fa = ca.first().cloned();
        let fb = cb.first().cloned();
        match (fa, fb) {
            (Some(fa), Some(fb)) if fa == fb => Ok(fa),
            (Some(_), Some(_)) => Err(EngineError::InvalidRange(format!(
                "{op}: anchors {}→{} resolve to different files; both must be in the same file",
                a.to_str(),
                b.to_str()
            ))),
            (f, _) => Err(unknown_anchor_of(if f.is_none() { a } else { b }, op)),
        }
    }

    /// Anchor-addressed replace: `remove_from`/`remove_to` resolve to one
    /// file.
    pub fn replace_anchors(
        &self,
        remove_from: Anchor,
        remove_to: Anchor,
        replacement: &str,
    ) -> Result<(PathBuf, Vec<DiffRow>), EngineError> {
        let path = self.resolve_same_file(remove_from, remove_to, "replace")?;
        self.touch(&path);
        let rows = self.replace(&path, remove_from, remove_to, replacement)?;
        Ok((path, rows))
    }

    /// Anchor-addressed replace_within.
    pub fn replace_within_anchors(
        &self,
        replace_from: Anchor,
        replace_to: Anchor,
        old: &str,
        new: &str,
    ) -> Result<(PathBuf, Vec<DiffRow>), EngineError> {
        let path = self.resolve_same_file(replace_from, replace_to, "replace_within")?;
        self.touch(&path);
        let rows = self.replace_within(&path, replace_from, replace_to, old, new)?;
        Ok((path, rows))
    }

    /// Anchor-addressed insert.
    pub fn insert_anchors(
        &self,
        anchor: Anchor,
        direction: Direction,
        text: &str,
    ) -> Result<(PathBuf, Vec<DiffRow>), EngineError> {
        let cands = self.resolve_anchor(anchor);
        let path = cands
            .first()
            .cloned()
            .ok_or_else(|| unknown_anchor_of(anchor, "insert"))?;
        self.touch(&path);
        let rows = self.insert(&path, anchor, direction, text)?;
        Ok((path, rows))
    }

    /// Copy a range to another position; `source_*` anchors resolve the
    /// source file, `insert_after` the destination (possibly the same file).
    pub fn copy_anchors(
        &self,
        source_from: Anchor,
        source_to: Anchor,
        insert_after: Anchor,
    ) -> Result<(PathBuf, PathBuf, Vec<DiffRow>), EngineError> {
        let src = self.resolve_same_file(source_from, source_to, "copy")?;
        let dst = self
            .resolve_anchor(insert_after)
            .first()
            .cloned()
            .ok_or_else(|| unknown_anchor_of(insert_after, "copy"))?;
        self.touch(&src);
        self.touch(&dst);
        let rows = self.copy(&src, source_from, source_to, &dst, insert_after)?;
        Ok((src, dst, rows))
    }

    /// Move a range; same resolution rules as copy.
    pub fn move_anchors(
        &self,
        source_from: Anchor,
        source_to: Anchor,
        insert_after: Anchor,
    ) -> Result<(PathBuf, PathBuf, Vec<DiffRow>, Vec<DiffRow>), EngineError> {
        let src = self.resolve_same_file(source_from, source_to, "move")?;
        let dst = self
            .resolve_anchor(insert_after)
            .first()
            .cloned()
            .ok_or_else(|| unknown_anchor_of(insert_after, "move"))?;
        self.touch(&src);
        self.touch(&dst);
        let (removed, added) = self.move_range(&src, source_from, source_to, &dst, insert_after)?;
        Ok((src, dst, removed, added))
    }

    pub fn home(&self) -> &Path {
        &self.home
    }
}

/// Canonical store key for a path: the same file spelled differently
/// (`./a.rs`, `A.RS`, relative vs absolute) must share ONE map and ONE
/// sidecar, or edits through one spelling silently miss the other's.
pub(crate) fn norm_key(path: &Path) -> PathBuf {
    let input = feather_core::workspace_path::normalize_input(path);
    let cand = std::fs::canonicalize(&input)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| {
            if input.is_absolute() {
                input.to_string_lossy().into_owned()
            } else {
                std::env::current_dir()
                    .unwrap_or_default()
                    .join(&input)
                    .to_string_lossy()
                    .into_owned()
            }
        });
    #[cfg(windows)]
    {
        // NTFS is case-insensitive: fold case + drop the verbatim prefix
        let s = cand.strip_prefix(r"\\?\").unwrap_or(&cand).to_lowercase();
        PathBuf::from(s)
    }
    #[cfg(not(windows))]
    {
        PathBuf::from(cand)
    }
}

fn unknown_anchor_of(a: Anchor, op: &str) -> EngineError {
    EngineError::UnknownAnchor(format!(
        "{op}: {} is not a live anchor in any mapped file — read the file (or anchor_grep) first to get fresh anchors",
        a.to_str()
    ))
}

fn bad_anchor(a: Anchor) -> EngineError {
    unknown_anchor_of(a, "op")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "feather-store-{tag}-{}-{}",
            std::process::id(),
            feather_core::message::now_millis()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn anchor_edits_outside_workspace_rewind_through_session() {
        use feather_core::{Message, Session};
        let h = home("external-history");
        let cwd = h.join("workspace");
        std::fs::create_dir_all(&cwd).unwrap();
        let mut session = Session::new(&h, &cwd).unwrap();
        let external = h.join("outside.txt");
        std::fs::write(&external, "before\n").unwrap();
        let created = h.join("new-dir/new.txt");
        let store =
            AnchorStore::new(&session.dir().join("anchors")).with_snapshots(session.dir(), &cwd);
        session.push(Message::user_text("baseline")).unwrap();
        session.commit_turn().unwrap();
        store.read(&external, None, None).unwrap();
        let anchor = store.build_map(&external).unwrap().anchor_at(0).unwrap();
        store.replace(&external, anchor, anchor, "after").unwrap();
        store.write(&created, "created").unwrap();
        session.push(Message::user_text("external edit")).unwrap();
        session.commit_turn().unwrap();
        session.rewind_to(1).unwrap();
        assert_eq!(std::fs::read_to_string(&external).unwrap(), "before\n");
        assert!(!created.exists());
        session.rewind_to(2).unwrap();
        assert_eq!(std::fs::read_to_string(&external).unwrap(), "after\n");
        assert!(created.exists());
        drop(session);
        std::fs::remove_dir_all(h).unwrap();
    }
    #[test]
    fn mapping_survives_restart_via_sidecar() {
        let h = home("restart");
        let f = h.join("code.rs");
        std::fs::write(&f, "fn main() {}\n// done\n").unwrap();

        let s1 = AnchorStore::new(&h);
        let m1 = s1.build_map(&f).unwrap();
        let anchors1: Vec<String> = (0..m1.len())
            .map(|i| m1.anchor_at(i).unwrap().to_str())
            .collect();

        let s2 = AnchorStore::new(&h); // "restarted"
        let m2 = s2.build_map(&f).unwrap();
        let anchors2: Vec<String> = (0..m2.len())
            .map(|i| m2.anchor_at(i).unwrap().to_str())
            .collect();
        assert_eq!(anchors1, anchors2);
        let _ = std::fs::remove_dir_all(&h);
    }

    #[test]
    fn pre_upgrade_record_is_ignored_and_restamped() {
        // A record written by an older build holds 4-letter anchors, which no
        // longer parse. It must not break the file, must not be trusted, and
        // must be replaced with a current-format record.
        let h = home("upgrade");
        let f = h.join("code.rs");
        std::fs::write(&f, "alpha\nbeta\n").unwrap();

        let s1 = AnchorStore::new(&h);
        let m1 = s1.build_map(&f).unwrap();
        let anchors1: Vec<String> = (0..m1.len())
            .map(|i| m1.anchor_at(i).unwrap().to_str())
            .collect();

        // Overwrite the persisted record with an old-format one (correct hashes,
        // unparseable anchors) — exactly the shape a 4-letter build left behind.
        let records = feather_core::records::Records::open(&h.join("anchors.redb")).unwrap();
        let old = crate::engine::PersistRecord {
            path: f.display().to_string(),
            anchors: vec!["abcd".to_string(); 2],
            hashes: ["alpha", "beta"]
                .iter()
                .map(|l| feather_core::message::fnv1a(l.as_bytes()))
                .collect(),
            free: vec![],
            seed: 42,
        };
        records
            .put(&norm_key(&f).display().to_string(), &old)
            .unwrap();

        // Re-open: the stale record is rejected, a fresh mapping is minted and
        // written back, and the file stays fully addressable.
        let s2 = AnchorStore::new(&h);
        let rows = s2.read(&f, None, None).unwrap();
        assert_eq!(rows.len(), 2);
        for row in &rows {
            let a = row.split('│').next().unwrap();
            assert_eq!(a.len(), crate::engine::ANCHOR_LEN);
            assert!(a.chars().all(|c: char| c.is_ascii_lowercase()));
            assert!(crate::engine::Anchor::parse(a).is_some());
        }

        // A second restart now adopts the restamped record: stable anchors.
        let s3 = AnchorStore::new(&h);
        let m3 = s3.build_map(&f).unwrap();
        let anchors3: Vec<String> = (0..m3.len())
            .map(|i| m3.anchor_at(i).unwrap().to_str())
            .collect();
        assert_eq!(anchors3, anchors1, "restamped record must round-trip");

        let _ = std::fs::remove_dir_all(&h);
    }
    #[test]
    fn read_replace_undo_flow() {
        let h = home("flow");
        let f = h.join("t.txt");
        std::fs::write(&f, "alpha\nbeta\ngamma\n").unwrap();
        let s = AnchorStore::new(&h);
        let rows = s.read(&f, None, None).unwrap();
        assert_eq!(rows.len(), 3);
        let a0 = Anchor::parse(rows[0].split_once('│').unwrap().0).unwrap();

        let diff = s.replace(&f, a0, a0, "ALPHA").unwrap();
        // pi-style diff: 2 churn rows (Removed+Added) + Kept context + Ellipsis
        assert_eq!(diff.len(), 4);
        assert!(matches!(diff[0].kind, crate::DiffKind::Removed));
        assert!(matches!(diff[1].kind, crate::DiffKind::Added));
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "ALPHA\nbeta\ngamma\n");

        s.undo(&f).unwrap();
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "alpha\nbeta\ngamma\n");
        // second undo is stale
        assert!(matches!(s.undo(&f), Err(EngineError::UndoStale)));
        let _ = std::fs::remove_dir_all(&h);
    }

    #[test]
    fn edit_keeps_unchanged_lines_stable() {
        let h = home("stable");
        let f = h.join("t.txt");
        std::fs::write(&f, "a\nb\nc\n").unwrap();
        let s = AnchorStore::new(&h);
        let rows = s.read(&f, None, None).unwrap();
        let a0 = Anchor::parse(rows[0].split_once('│').unwrap().0).unwrap();
        let a2 = Anchor::parse(rows[2].split_once('│').unwrap().0).unwrap();
        s.replace(&f, a0, a0, "A1").unwrap();
        let rows2 = s.read(&f, None, None).unwrap();
        // line 3's anchor survived the edit
        let a2b = Anchor::parse(rows2[2].split_once('│').unwrap().0).unwrap();
        assert_eq!(a2, a2b);
        assert_eq!(rows2[2], format!("{a2b}│c"));
        let _ = std::fs::remove_dir_all(&h);
    }

    #[test]
    fn cross_file_copy_move() {
        let h = home("xfile");
        let fa = h.join("a.txt");
        let fb = h.join("b.txt");
        std::fs::write(&fa, "s1\ns2\ns3\n").unwrap();
        std::fs::write(&fb, "d1\n").unwrap();
        let s = AnchorStore::new(&h);
        let ra = s.read(&fa, None, None).unwrap();
        let rb = s.read(&fb, None, None).unwrap();
        let sf = Anchor::parse(ra[0].split_once('│').unwrap().0).unwrap();
        let st = Anchor::parse(ra[1].split_once('│').unwrap().0).unwrap();
        let di = Anchor::parse(rb[0].split_once('│').unwrap().0).unwrap();

        // copy
        s.copy(&fa, sf, st, &fb, di).unwrap();
        assert_eq!(std::fs::read_to_string(&fb).unwrap(), "d1\ns1\ns2\n");
        assert_eq!(std::fs::read_to_string(&fa).unwrap(), "s1\ns2\ns3\n");

        // move
        let _ = s.move_range(&fa, sf, st, &fb, di).unwrap();
        assert_eq!(std::fs::read_to_string(&fa).unwrap(), "s3\n");
        assert_eq!(
            std::fs::read_to_string(&fb).unwrap(),
            "d1\ns1\ns2\ns1\ns2\n"
        );
        let _ = std::fs::remove_dir_all(&h);
    }

    #[test]
    fn binary_refused() {
        let h = home("bin");
        let f = h.join("bin.dat");
        std::fs::write(&f, [0x00, 0x01, 0x02]).unwrap();
        let s = AnchorStore::new(&h);
        assert!(matches!(
            s.read(&f, None, None),
            Err(EngineError::Binary(_))
        ));
        let _ = std::fs::remove_dir_all(&h);
    }
}
