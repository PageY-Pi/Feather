//! Anchor-addressed editing engine (pi-hashline-edit-pro semantics).
//!
//! Every line a tool serves is prefixed with a unique 4-character anchor:
//!
//! ```text
//! Hasu│old text
//! arvm│old text 2
//! ```
//!
//! Edits are addressed by anchor, never by line number, and nothing is
//! fuzzy-matched, so an edit lands on the line it meant. Properties:
//!
//! - **4-char, letters only, case-sensitive** — 52^4 anchors per file; the
//!   alphabet avoids digits/symbols so anchors tokenize cheaply.
//! - **Stable mapping** — unchanged lines keep their anchors across edits;
//!   freed anchors (deleted lines) are reused LIFO; new lines draw fresh
//!   anchors from a deterministic per-file stream. The mapping persists to
//!   a sidecar so anchors survive restarts.
//! - **Stale detection** — if a served line's content no longer matches the
//!   recorded hash, edits are refused with `[E_ANCHOR_STALE]` and the
//!   current rows are served back; nothing is guessed.
//! - **No fuzzy anything** — `replace_within` demands an exact single
//!   occurrence; misses refuse with the current rows attached.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

pub const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ";
pub const SPACE: u64 = (ALPHABET.len() as u64)
    * (ALPHABET.len() as u64)
    * (ALPHABET.len() as u64)
    * (ALPHABET.len() as u64);

/// Max bytes of a file the engine will map.
pub const MAX_MAP_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Anchor(pub u32);

impl Anchor {
    pub fn from_index(i: u64) -> Anchor {
        Anchor((i % SPACE) as u32)
    }

    pub fn to_str(self) -> String {
        self.to_string()
    }

    pub fn parse(s: &str) -> Option<Anchor> {
        if s.len() != 4 {
            return None;
        }
        let mut i: u64 = 0;
        for b in s.bytes() {
            let pos = ALPHABET.iter().position(|&a| a == b)? as u64;
            i = i * ALPHABET.len() as u64 + pos;
        }
        Some(Anchor(i as u32))
    }
}

impl std::fmt::Display for Anchor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut i = self.0 as u64;
        let mut buf = [0u8; 4];
        for slot in buf.iter_mut().rev() {
            *slot = ALPHABET[(i % ALPHABET.len() as u64) as usize];
            i /= ALPHABET.len() as u64;
        }
        f.write_str(std::str::from_utf8(&buf).expect("alphabet is ascii"))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffKind {
    Removed,
    Added,
    Kept,
    /// ` ... ` marker: more file exists beyond the diff's context rows.
    Ellipsis,
}

#[derive(Debug, Clone)]
pub struct DiffRow {
    pub kind: DiffKind,
    pub anchor: Anchor,
    pub content: String,
}

/// Pi-style diff framing: one `Kept` row hugging the churn on each side,
/// with ` ... ` markers whenever more file lies beyond the context.
fn framed(
    before: Option<(Anchor, String)>,
    more_before: bool,
    churn: Vec<DiffRow>,
    after: Option<(Anchor, String)>,
    more_after: bool,
) -> Vec<DiffRow> {
    let mut out: Vec<DiffRow> = Vec::with_capacity(churn.len() + 4);
    if let Some((anchor, content)) = &before {
        if more_before {
            out.push(DiffRow {
                kind: DiffKind::Ellipsis,
                anchor: *anchor,
                content: String::new(),
            });
        }
        out.push(DiffRow {
            kind: DiffKind::Kept,
            anchor: *anchor,
            content: content.clone(),
        });
    }
    out.extend(churn);
    if let Some((anchor, content)) = &after {
        out.push(DiffRow {
            kind: DiffKind::Kept,
            anchor: *anchor,
            content: content.clone(),
        });
        if more_after {
            out.push(DiffRow {
                kind: DiffKind::Ellipsis,
                anchor: *anchor,
                content: String::new(),
            });
        }
    }
    out
}

impl DiffRow {
    pub fn render(&self) -> String {
        if matches!(self.kind, DiffKind::Ellipsis) {
            return " ... ".to_string();
        }
        let sign = match self.kind {
            DiffKind::Removed => '-',
            DiffKind::Added => '+',
            DiffKind::Kept | DiffKind::Ellipsis => ' ',
        };
        format!("{sign}{}│{}", self.anchor.to_str(), self.content)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("[E_UNKNOWN_ANCHOR] {0}")]
    UnknownAnchor(String),
    #[error("[E_ANCHOR_STALE] {0}")]
    AnchorStale(String),
    #[error("[E_RANGE_ORDER] remove_from must come before remove_to: {0}")]
    RangeOrder(String),
    #[error(
        "[E_MATCH_COUNT] replace_old must occur exactly once in the range, found {count}: {detail}"
    )]
    MatchCount { count: usize, detail: String },
    #[error("[E_INVALID_RANGE] {0}")]
    InvalidRange(String),
    #[error("[E_IO] {0}")]
    Io(String),
    #[error("[E_BINARY] {0}")]
    Binary(String),
    #[error("[E_TOO_LARGE] {0}")]
    TooLarge(String),
    #[error("[E_UNDO_STALE] nothing to undo for this file (or the file changed since)")]
    UndoStale,
    #[error("[E_NO_CHANGE] {0}")]
    NoChange(String),
    #[error("[E_ENCODE] {0}")]
    Encode(String),
}

/// Deterministic xorshift64* stream, seeded per file.
pub struct AnchorRng(u64);

impl AnchorRng {
    pub fn new(seed: u64) -> Self {
        Self(seed.max(1))
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545F4914F6CDD1D)
    }
}

/// One undo step: the full pre-edit state of a file.
#[derive(Clone)]
struct UndoSnapshot {
    lines: Vec<String>,
    anchors: Vec<Anchor>,
    hashes: Vec<u64>,
    free: Vec<Anchor>,
    rng_state: u64,
}

/// The anchor mapping + content of one file.
pub struct AnchorMap {
    pub path: PathBuf,
    pub lines: Vec<String>,
    /// Whether the file's lines were CRLF-terminated on load (write-back
    /// preserves the detected convention; LF otherwise).
    pub crlf: bool,
    /// Whether the file ended with a trailing newline on load.
    pub trailing_newline: bool,
    anchors: Vec<Anchor>,
    hashes: Vec<u64>,
    by_anchor: HashMap<Anchor, usize>,
    free: Vec<Anchor>,
    rng: AnchorRng,
    used: HashSet<Anchor>,
    last_undo: Option<UndoSnapshot>,
    /// set between remove+paste of a same-file move: paste keeps the
    /// remove's snapshot so undo restores the PRE-move state as one step
    composite_undo: bool,
    generation: u64,
}

impl AnchorMap {
    /// Map a fresh file: allocate anchors for all lines from the per-file
    /// deterministic stream.
    pub fn new(path: PathBuf, content: String, seed: u64) -> Self {
        let crlf = content.contains("\r\n");
        let trailing_newline = content.ends_with('\n');
        let lines: Vec<String> = split_lines(&content);
        let mut rng = AnchorRng::new(seed);
        let mut used = HashSet::new();
        let mut anchors = Vec::with_capacity(lines.len());
        for _ in 0..lines.len() {
            let a = draw_fresh(&mut rng, &used);
            used.insert(a);
            anchors.push(a);
        }
        let hashes = lines
            .iter()
            .map(|l| feather_core::message::fnv1a(l.as_bytes()))
            .collect();
        let mut map = Self {
            path,
            lines,
            crlf,
            trailing_newline,
            anchors,
            hashes,
            by_anchor: HashMap::new(),
            free: Vec::new(),
            rng,
            used,
            last_undo: None,
            composite_undo: false,
            generation: 0,
        };
        map.reindex();
        map
    }

    fn reindex(&mut self) {
        self.by_anchor.clear();
        for (i, a) in self.anchors.iter().enumerate() {
            self.by_anchor.insert(*a, i);
        }
    }

    pub fn len(&self) -> usize {
        self.lines.len()
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn anchor_at(&self, idx: usize) -> Option<Anchor> {
        self.anchors.get(idx).copied()
    }

    pub fn line_index(&self, anchor: Anchor) -> Option<usize> {
        self.by_anchor.get(&anchor).copied()
    }

    /// Render rows `anchor│content` for a 0-indexed inclusive range,
    /// validating recorded hashes against current content.
    pub fn serve_rows(&self, from: usize, to: usize) -> Vec<String> {
        let to = to.min(self.lines.len().saturating_sub(1));
        let from = from.min(self.lines.len());
        let mut out = Vec::with_capacity(to.saturating_sub(from) + 1);
        if self.lines.is_empty() {
            return out;
        }
        for i in from..=to {
            out.push(format!("{}│{}", self.anchors[i].to_str(), self.lines[i]));
        }
        out
    }

    /// Current rows for the lines an anchor range now points at (used in
    /// error messages).
    pub fn rows_for_anchor_range(&self, from: Anchor, to: Anchor) -> Option<Vec<String>> {
        let a = self.line_index(from)?;
        let b = self.line_index(to)?;
        if a > b {
            return None;
        }
        Some(self.serve_rows(a, b))
    }

    fn snapshot(&self) -> UndoSnapshot {
        UndoSnapshot {
            lines: self.lines.clone(),
            anchors: self.anchors.clone(),
            hashes: self.hashes.clone(),
            free: self.free.clone(),
            rng_state: self.rng.0,
        }
    }

    fn restore(&mut self, snap: UndoSnapshot) {
        self.lines = snap.lines;
        self.anchors = snap.anchors;
        self.hashes = snap.hashes;
        self.free = snap.free;
        self.rng.0 = snap.rng_state;
        self.used = self
            .anchors
            .iter()
            .chain(self.free.iter())
            .copied()
            .collect();
        self.reindex();
        self.generation += 1;
        self.last_undo = None; // single-level undo per file
    }

    fn take_anchor(&mut self) -> Anchor {
        // Popping from the free table hands the anchor to a NEW line, but the
        // anchor must STAY in `used`: it now names a live row, and removing it
        // would let draw_fresh() re-issue it to another row (duplicate anchors
        // => silent wrong-line edits). Invariant: used == active ∪ free.
        if let Some(a) = self.free.pop() {
            return a;
        }
        draw_fresh(&mut self.rng, &self.used)
    }

    fn release_anchor(&mut self, a: Anchor) {
        self.free.push(a);
    }

    /// Verify a line's recorded hash still matches its content.
    pub fn verify_line(&self, idx: usize) -> bool {
        match (self.lines.get(idx), self.hashes.get(idx)) {
            (Some(l), Some(h)) => feather_core::message::fnv1a(l.as_bytes()) == *h,
            _ => false,
        }
    }

    /// Parse replacement text into lines (trailing-break rule: one trailing
    /// `\n` sets the last line's ending instead of adding a blank line).
    fn parse_lines(text: &str) -> Vec<String> {
        if text.is_empty() {
            return Vec::new();
        }
        let mut parts: Vec<String> = text.split('\n').map(str::to_string).collect();
        if text.ends_with('\n') && parts.len() > 1 {
            parts.pop();
        }
        parts
    }

    // ------------------------------------------------------------------
    // replace: swap the inclusive anchor range for replacement_lines
    // ------------------------------------------------------------------
    pub fn replace(
        &mut self,
        remove_from: Anchor,
        remove_to: Anchor,
        replacement: &str,
    ) -> Result<Vec<DiffRow>, EngineError> {
        let a = self
            .line_index(remove_from)
            .ok_or_else(|| unknown(remove_from))?;
        let b = self
            .line_index(remove_to)
            .ok_or_else(|| unknown(remove_to))?;
        if a > b {
            return Err(EngineError::RangeOrder(format!(
                "{}..={} ({} → {})",
                a,
                b,
                remove_from.to_str(),
                remove_to.to_str()
            )));
        }
        // stale check across the whole removal range
        let mut stale = None;
        for i in a..=b {
            if !self.verify_line(i) {
                stale = Some(i);
                break;
            }
        }
        if let Some(i) = stale {
            return Err(EngineError::AnchorStale(format!(
                "line {} changed since it was served; current rows:\n{}",
                i + 1,
                self.serve_rows(a, b).join("\n")
            )));
        }

        let snap = self.snapshot();
        let new_lines = Self::parse_lines(replacement);
        // context rows for the pi-style diff (original coords, before splice)
        let ctx_before = if a > 0 {
            Some((self.anchors[a - 1], self.lines[a - 1].clone()))
        } else {
            None
        };
        let more_before = a > 1;
        let ctx_after = if b + 1 < self.lines.len() {
            Some((self.anchors[b + 1], self.lines[b + 1].clone()))
        } else {
            None
        };
        let more_after = b + 2 < self.lines.len();
        let mut diff: Vec<DiffRow> = Vec::new();
        for i in a..=b {
            diff.push(DiffRow {
                kind: DiffKind::Removed,
                anchor: self.anchors[i],
                content: self.lines[i].clone(),
            });
        }
        // splice
        // the untouched tail carries its hashes — no recompute below
        let tail: Vec<(Anchor, String, u64)> = self
            .anchors
            .drain(b + 1..)
            .zip(self.lines.drain(b + 1..))
            .zip(self.hashes.drain(b + 1..))
            .map(|((anchor, line), hash)| (anchor, line, hash))
            .collect();
        self.anchors.truncate(a);
        self.lines.truncate(a);
        self.hashes.truncate(a);
        for content in new_lines {
            let anchor = self.take_anchor();
            self.anchors.push(anchor);
            self.hashes
                .push(feather_core::message::fnv1a(content.as_bytes()));
            diff.push(DiffRow {
                kind: DiffKind::Added,
                anchor,
                content: content.clone(),
            });
            self.lines.push(content);
        }
        for (anchor, content, hash) in tail {
            self.anchors.push(anchor);
            self.lines.push(content);
            self.hashes.push(hash);
        }
        debug_assert_eq!(self.hashes.len(), self.lines.len());
        self.reindex();
        self.generation += 1;
        self.last_undo = Some(snap);
        self.write_back()?;
        Ok(framed(ctx_before, more_before, diff, ctx_after, more_after))
    }

    // ------------------------------------------------------------------
    // replace_within: change part of a line (or range) without retyping
    // the rest
    // ------------------------------------------------------------------
    pub fn replace_within(
        &mut self,
        replace_from: Anchor,
        replace_to: Anchor,
        replace_old: &str,
        replace_new: &str,
    ) -> Result<Vec<DiffRow>, EngineError> {
        let a = self
            .line_index(replace_from)
            .ok_or_else(|| unknown(replace_from))?;
        let b = self
            .line_index(replace_to)
            .ok_or_else(|| unknown(replace_to))?;
        if a > b {
            return Err(EngineError::RangeOrder(format!("{a}..={b}")));
        }
        for i in a..=b {
            if !self.verify_line(i) {
                return Err(EngineError::AnchorStale(format!(
                    "line {} changed since it was served; current rows:\n{}",
                    i + 1,
                    self.serve_rows(a, b).join("\n")
                )));
            }
        }
        let joined = self.lines[a..=b].join("\n");
        let count = count_occurrences(&joined, replace_old);
        match count {
            0 => {
                return Err(EngineError::MatchCount {
                    count: 0,
                    detail: format!(
                        "not found in range; current rows:\n{}",
                        self.serve_rows(a, b).join("\n")
                    ),
                })
            }
            1 => {}
            n => {
                // serve the matching lines with numbers so the caller can narrow
                let rows = self.serve_rows(a, b).join("\n");
                return Err(EngineError::MatchCount {
                    count: n,
                    detail: format!(
                        "repeated match; search narrower with anchor_grep. rows:\n{rows}"
                    ),
                });
            }
        }

        let snap = self.snapshot();
        // context rows for the pi-style diff (original coords, before splice)
        let ctx_before = if a > 0 {
            Some((self.anchors[a - 1], self.lines[a - 1].clone()))
        } else {
            None
        };
        let more_before = a > 1;
        let ctx_after = if b + 1 < self.lines.len() {
            Some((self.anchors[b + 1], self.lines[b + 1].clone()))
        } else {
            None
        };
        let more_after = b + 2 < self.lines.len();
        let idx = joined.find(replace_old).expect("occurrence checked");
        let mut rebuilt = String::with_capacity(joined.len());
        rebuilt.push_str(&joined[..idx]);
        rebuilt.push_str(replace_new);
        rebuilt.push_str(&joined[idx + replace_old.len()..]);
        let new_lines: Vec<String> = rebuilt.split('\n').map(str::to_string).collect();

        let mut diff: Vec<DiffRow> = Vec::new();
        for i in a..=b {
            diff.push(DiffRow {
                kind: DiffKind::Removed,
                anchor: self.anchors[i],
                content: self.lines[i].clone(),
            });
        }
        // the untouched tail carries its hashes — no recompute below
        let tail: Vec<(Anchor, String, u64)> = self
            .anchors
            .drain(b + 1..)
            .zip(self.lines.drain(b + 1..))
            .zip(self.hashes.drain(b + 1..))
            .map(|((anchor, line), hash)| (anchor, line, hash))
            .collect();
        self.anchors.truncate(a);
        self.lines.truncate(a);
        self.hashes.truncate(a);
        for content in new_lines {
            let anchor = self.take_anchor();
            self.anchors.push(anchor);
            self.hashes
                .push(feather_core::message::fnv1a(content.as_bytes()));
            diff.push(DiffRow {
                kind: DiffKind::Added,
                anchor,
                content: content.clone(),
            });
            self.lines.push(content);
        }
        for (anchor, content, hash) in tail {
            self.anchors.push(anchor);
            self.lines.push(content);
            self.hashes.push(hash);
        }
        debug_assert_eq!(self.hashes.len(), self.lines.len());
        self.reindex();
        self.generation += 1;
        self.last_undo = Some(snap);
        self.write_back()?;
        Ok(framed(ctx_before, more_before, diff, ctx_after, more_after))
    }

    // ------------------------------------------------------------------
    // insert: add lines next to one existing anchor line
    // ------------------------------------------------------------------
    pub fn insert(
        &mut self,
        anchor: Anchor,
        direction: Direction,
        text: &str,
    ) -> Result<Vec<DiffRow>, EngineError> {
        let i = self.line_index(anchor).ok_or_else(|| unknown(anchor))?;
        if !self.verify_line(i) {
            return Err(EngineError::AnchorStale(format!(
                "line {} changed since it was served; current rows:\n{}",
                i + 1,
                self.serve_rows(i, i).join("\n")
            )));
        }
        let snap = self.snapshot();
        // parse with the mirror rule for `before`
        let mut new_lines = Self::parse_lines(text);
        if text.is_empty() {
            new_lines = vec![String::new()]; // one blank line
        }
        if direction == Direction::Before && text.starts_with('\n') && new_lines.len() > 1 {
            new_lines.remove(0);
        } else if direction == Direction::Before && text == "\n" {
            new_lines = vec![String::new()];
        }
        let at = match direction {
            Direction::After => i + 1,
            Direction::Before => i,
        };
        // context rows for the pi-style diff (original coords, before splice)
        let ctx_before = if at > 0 {
            Some((self.anchors[at - 1], self.lines[at - 1].clone()))
        } else {
            None
        };
        let more_before = at > 1;
        let ctx_after = if at < self.lines.len() {
            Some((self.anchors[at], self.lines[at].clone()))
        } else {
            None
        };
        let more_after = at + 1 < self.lines.len();
        let mut diff: Vec<DiffRow> = Vec::new();
        // the untouched tail carries its hashes — no recompute below
        let tail: Vec<(Anchor, String, u64)> = self
            .anchors
            .drain(at..)
            .zip(self.lines.drain(at..))
            .zip(self.hashes.drain(at..))
            .map(|((anchor, line), hash)| (anchor, line, hash))
            .collect();
        self.hashes.truncate(at);
        for content in new_lines {
            let anchor = self.take_anchor();
            self.anchors.push(anchor);
            self.hashes
                .push(feather_core::message::fnv1a(content.as_bytes()));
            diff.push(DiffRow {
                kind: DiffKind::Added,
                anchor,
                content: content.clone(),
            });
            self.lines.push(content);
        }
        for (anchor, content, hash) in tail {
            self.anchors.push(anchor);
            self.lines.push(content);
            self.hashes.push(hash);
        }
        debug_assert_eq!(self.hashes.len(), self.lines.len());
        self.reindex();
        self.generation += 1;
        self.last_undo = Some(snap);
        self.write_back()?;
        Ok(framed(ctx_before, more_before, diff, ctx_after, more_after))
    }

    // ------------------------------------------------------------------
    // copy / move a range of lines to another position (possibly another
    // file's map, handled by the store)
    // ------------------------------------------------------------------
    pub fn take_range(
        &mut self,
        from: Anchor,
        to: Anchor,
    ) -> Result<Vec<(Anchor, String, u64)>, EngineError> {
        let a = self.line_index(from).ok_or_else(|| unknown(from))?;
        let b = self.line_index(to).ok_or_else(|| unknown(to))?;
        if a > b {
            return Err(EngineError::RangeOrder(format!("{a}..={b}")));
        }
        let taken: Vec<(Anchor, String, u64)> = (a..=b)
            .map(|i| (self.anchors[i], self.lines[i].clone(), self.hashes[i]))
            .collect();
        Ok(taken)
    }

    /// Remove a range (used by cross-map move after the payload was taken).
    pub fn remove_range(&mut self, from: Anchor, to: Anchor) -> Result<Vec<DiffRow>, EngineError> {
        let a = self.line_index(from).ok_or_else(|| unknown(from))?;
        let b = self.line_index(to).ok_or_else(|| unknown(to))?;
        if a > b {
            return Err(EngineError::RangeOrder(format!("{a}..={b}")));
        }
        let snap = self.snapshot();
        // context rows for the pi-style diff (original coords, before splice)
        let ctx_before = if a > 0 {
            Some((self.anchors[a - 1], self.lines[a - 1].clone()))
        } else {
            None
        };
        let more_before = a > 1;
        let ctx_after = if b + 1 < self.lines.len() {
            Some((self.anchors[b + 1], self.lines[b + 1].clone()))
        } else {
            None
        };
        let more_after = b + 2 < self.lines.len();
        let mut diff: Vec<DiffRow> = Vec::new();
        let mut freed: Vec<Anchor> = Vec::new();
        for i in a..=b {
            diff.push(DiffRow {
                kind: DiffKind::Removed,
                anchor: self.anchors[i],
                content: self.lines[i].clone(),
            });
            freed.push(self.anchors[i]);
        }
        // the untouched tail carries its hashes — no recompute below
        let tail: Vec<(Anchor, String, u64)> = self
            .anchors
            .drain(b + 1..)
            .zip(self.lines.drain(b + 1..))
            .zip(self.hashes.drain(b + 1..))
            .map(|((anchor, line), hash)| (anchor, line, hash))
            .collect();
        self.anchors.truncate(a);
        self.lines.truncate(a);
        self.hashes.truncate(a);
        for anchor in freed {
            self.release_anchor(anchor);
        }
        for (anchor, content, hash) in tail {
            self.anchors.push(anchor);
            self.lines.push(content);
            self.hashes.push(hash);
        }
        debug_assert_eq!(self.hashes.len(), self.lines.len());
        if self.lines.is_empty() {
            // keep one empty row so the file stays addressable
            let anchor = self.take_anchor();
            self.anchors.push(anchor);
            self.lines.push(String::new());
            self.hashes.push(feather_core::message::fnv1a(b""));
            diff.push(DiffRow {
                kind: DiffKind::Added,
                anchor,
                content: String::new(),
            });
        }
        self.reindex();
        self.generation += 1;
        self.last_undo = Some(snap);
        self.write_back()?;
        Ok(framed(ctx_before, more_before, diff, ctx_after, more_after))
    }

    /// Insert payload lines after `insert_after` (destination side of
    /// copy/move). When `keep_anchors` is set the payload anchors are
    /// preserved (same-file move); otherwise fresh anchors are drawn.
    pub fn paste_range(
        &mut self,
        insert_after: Anchor,
        payload: &[(Anchor, String, u64)],
        keep_anchors: bool,
    ) -> Result<Vec<DiffRow>, EngineError> {
        let i = self
            .line_index(insert_after)
            .ok_or_else(|| unknown(insert_after))?;
        if !self.verify_line(i) {
            return Err(EngineError::AnchorStale(format!(
                "line {} changed since it was served; current rows:\n{}",
                i + 1,
                self.serve_rows(i, i).join("\n")
            )));
        }
        let snap = self.snapshot();
        let at = i + 1;
        // context rows for the pi-style diff (original coords, before splice)
        let ctx_before = if at > 0 {
            Some((self.anchors[at - 1], self.lines[at - 1].clone()))
        } else {
            None
        };
        let more_before = at > 1;
        let ctx_after = if at < self.lines.len() {
            Some((self.anchors[at], self.lines[at].clone()))
        } else {
            None
        };
        let more_after = at + 1 < self.lines.len();
        let mut diff: Vec<DiffRow> = Vec::new();
        // the untouched tail carries its hashes — no recompute below
        let tail: Vec<(Anchor, String, u64)> = self
            .anchors
            .drain(at..)
            .zip(self.lines.drain(at..))
            .zip(self.hashes.drain(at..))
            .map(|((anchor, line), hash)| (anchor, line, hash))
            .collect();
        self.hashes.truncate(at);
        for (old_anchor, content, hash) in payload {
            // keep the payload anchor only when the free table holds it
            // (released by remove_range) — `used` alone can't tell "free"
            // from "live on another row"
            let anchor = if keep_anchors && self.free.contains(old_anchor) {
                self.free.retain(|a| a != old_anchor);
                *old_anchor
            } else {
                self.take_anchor()
            };
            self.anchors.push(anchor);
            self.lines.push(content.clone());
            self.hashes.push(*hash);
            diff.push(DiffRow {
                kind: DiffKind::Added,
                anchor,
                content: content.clone(),
            });
        }
        for (anchor, content, hash) in tail {
            self.anchors.push(anchor);
            self.lines.push(content);
            self.hashes.push(hash);
        }
        debug_assert_eq!(self.hashes.len(), self.lines.len());
        self.reindex();
        self.generation += 1;
        if !self.composite_undo {
            self.last_undo = Some(snap);
        }
        self.write_back()?;
        Ok(framed(ctx_before, more_before, diff, ctx_after, more_after))
    }

    /// Begin a composite edit (same-file move = remove + paste): the
    /// intermediate paste_range does not overwrite the undo snapshot, so a
    /// single undo restores the pre-move state instead of a torn half-move.
    pub fn begin_composite(&mut self) {
        self.composite_undo = true;
    }

    pub fn end_composite(&mut self) {
        self.composite_undo = false;
    }

    // ------------------------------------------------------------------
    // undo: restore the pre-edit state of the last mutation
    // ------------------------------------------------------------------
    pub fn undo(&mut self) -> Result<Vec<DiffRow>, EngineError> {
        let snap = self.last_undo.take().ok_or(EngineError::UndoStale)?;
        let old: Vec<(Anchor, String)> = self
            .anchors
            .iter()
            .cloned()
            .zip(self.lines.iter().cloned())
            .collect();
        self.restore(snap);
        let new: Vec<(Anchor, String)> = self
            .anchors
            .iter()
            .cloned()
            .zip(self.lines.iter().cloned())
            .collect();
        // exact churn: common prefix/suffix trimmed (a single edit — the
        // overwhelmingly common undo — collapses to its changed rows)
        let mut p = 0;
        while p < old.len() && p < new.len() && old[p] == new[p] {
            p += 1;
        }
        let mut suf = 0;
        while suf < old.len() - p
            && suf < new.len() - p
            && old[old.len() - 1 - suf] == new[new.len() - 1 - suf]
        {
            suf += 1;
        }
        let mut diff: Vec<DiffRow> = Vec::new();
        for (anchor, content) in &old[p..old.len() - suf] {
            diff.push(DiffRow {
                kind: DiffKind::Removed,
                anchor: *anchor,
                content: content.clone(),
            });
        }
        for (anchor, content) in &new[p..new.len() - suf] {
            diff.push(DiffRow {
                kind: DiffKind::Added,
                anchor: *anchor,
                content: content.clone(),
            });
        }
        // frame with the recovered rows hugging the churn
        let ctx_before = if p > 0 {
            Some(new[p - 1].clone())
        } else {
            None
        };
        let more_before = p > 1;
        let after_idx = new.len() - suf;
        let ctx_after = if after_idx < new.len() {
            Some(new[after_idx].clone())
        } else {
            None
        };
        let more_after = suf > 1;
        self.write_back()?;
        Ok(framed(ctx_before, more_before, diff, ctx_after, more_after))
    }

    pub fn can_undo(&self) -> bool {
        self.last_undo.is_some()
    }

    // ------------------------------------------------------------------
    // persistence
    // ------------------------------------------------------------------
    /// Serialize the current buffer content (applying detected EOL).
    pub fn to_file_content(&self) -> String {
        let sep = if self.crlf { "\r\n" } else { "\n" };
        let mut out = self.lines.join(sep);
        if self.trailing_newline && !self.lines.is_empty() {
            out.push_str(sep);
        }
        out
    }

    fn write_back(&self) -> Result<(), EngineError> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| EngineError::Io(e.to_string()))?;
        }
        std::fs::write(&self.path, self.to_file_content())
            .map_err(|e| EngineError::Io(e.to_string()))
    }

    /// Anchor + hash tables for sidecar persistence.
    pub fn persist_record(&self) -> PersistRecord {
        PersistRecord {
            path: self.path.display().to_string(),
            anchors: self.anchors.iter().map(|a| a.to_str()).collect(),
            hashes: self.hashes.clone(),
            free: self.free.iter().map(|a| a.to_str()).collect(),
            seed: self.rng.0,
        }
    }

    pub fn adopt_persist(&mut self, rec: PersistRecord) {
        if rec.anchors.len() != self.lines.len() || rec.hashes.len() != self.lines.len() {
            return;
        }
        let mut used = HashSet::new();
        let mut anchors = Vec::with_capacity(rec.anchors.len());
        for s in &rec.anchors {
            match Anchor::parse(s) {
                Some(a) if !used.contains(&a) => {
                    used.insert(a);
                    anchors.push(a);
                }
                _ => return, // corrupt record: keep computed mapping
            }
        }
        let mut free = Vec::new();
        for s in &rec.free {
            if let Some(a) = Anchor::parse(s) {
                if !used.contains(&a) {
                    used.insert(a);
                    free.push(a);
                }
            }
        }
        self.anchors = anchors;
        self.free = free;
        self.used = used;
        self.rng = AnchorRng::new(rec.seed.max(1));
        self.reindex();
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct PersistRecord {
    pub path: String,
    pub anchors: Vec<String>,
    pub hashes: Vec<u64>,
    pub free: Vec<String>,
    pub seed: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    After,
    Before,
}

fn draw_fresh(rng: &mut AnchorRng, used: &HashSet<Anchor>) -> Anchor {
    loop {
        let a = Anchor::from_index(rng.next_u64());
        if !used.contains(&a) {
            return a;
        }
    }
}

fn unknown(a: Anchor) -> EngineError {
    EngineError::UnknownAnchor(format!(
        "{} is not a live anchor in this file — read the file (or anchor_grep) to get fresh anchors",
        a.to_str()
    ))
}

pub fn split_lines(content: &str) -> Vec<String> {
    if content.is_empty() {
        return vec![String::new()]; // empty file: one empty, addressable row
    }
    let mut lines: Vec<String> = content.split('\n').map(str::to_string).collect();
    if content.ends_with('\n') {
        lines.pop();
    }
    // strip \r left by CRLF
    for l in lines.iter_mut() {
        if l.ends_with('\r') {
            l.pop();
        }
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

pub fn count_occurrences(haystack: &str, needle: &str) -> usize {
    if needle.is_empty() {
        return 0;
    }
    let mut n = 0;
    let mut from = 0;
    while let Some(pos) = haystack[from..].find(needle) {
        n += 1;
        from += pos + needle.len();
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map_with(content: &str) -> AnchorMap {
        let p = std::env::temp_dir().join(format!(
            "feather-hashline-test-{}-{}.txt",
            std::process::id(),
            feather_core::message::now_millis()
        ));
        AnchorMap::new(p, content.to_string(), 42)
    }

    fn anchor(m: &AnchorMap, idx: usize) -> Anchor {
        m.anchor_at(idx).unwrap()
    }

    #[test]
    fn anchor_roundtrip_all_letters() {
        for i in [0u64, 1, 51, 52, 12345, SPACE - 1] {
            let a = Anchor::from_index(i);
            let s = a.to_str();
            assert_eq!(s.len(), 4);
            assert_eq!(Anchor::parse(&s), Some(a));
        }
        assert!(Anchor::parse("0abc").is_none());
        assert!(Anchor::parse("abc").is_none());
    }

    #[test]
    fn replace_range_and_diff() {
        let mut m = map_with("alpha\nbeta\ngamma\ndelta\n");
        let a0 = anchor(&m, 0);
        let a2 = anchor(&m, 2);
        let diff = m.replace(a0, a2, "one\ntwo").unwrap();
        assert_eq!(
            diff.iter().filter(|d| d.kind == DiffKind::Removed).count(),
            3
        );
        assert_eq!(diff.iter().filter(|d| d.kind == DiffKind::Added).count(), 2);
        assert_eq!(m.lines, vec!["one", "two", "delta"]);
    }

    #[test]
    fn replace_trailing_newline_semantics() {
        let mut m = map_with("a\nb\nc\n");
        let a1 = anchor(&m, 1);
        // "x\n" → no blank between x and the next line
        m.replace(a1, a1, "x\n").unwrap();
        assert_eq!(m.lines, vec!["a", "x", "c"]);
        // re-anchor: the edited line's fresh anchor (stale a1 no longer live)
        let a1b = anchor(&m, 1);
        m.replace(a1b, a1b, "x\n\n").unwrap();
        assert_eq!(m.lines, vec!["a", "x", "", "c"]);
    }

    #[test]
    fn replace_empty_deletes_range() {
        let mut m = map_with("a\nb\nc\n");
        let a0 = anchor(&m, 0);
        let a1 = anchor(&m, 1);
        m.replace(a0, a1, "").unwrap();
        assert_eq!(m.lines, vec!["c"]);
    }

    #[test]
    fn replace_within_single_match() {
        let mut m = map_with("let size = \"small\";\nlet n = 1;\n");
        let a0 = anchor(&m, 0);
        let diff = m.replace_within(a0, a0, "small", "large").unwrap();
        assert_eq!(m.lines[0], "let size = \"large\";");
        // churn (removed + added) + the Kept row below; line 0 has no row
        // above, and the file ends at line 1, so no ` ... ` markers appear
        assert_eq!(diff.len(), 3);
        assert!(matches!(diff[0].kind, DiffKind::Removed));
        assert!(matches!(diff[1].kind, DiffKind::Added));
        assert!(matches!(diff[2].kind, DiffKind::Kept));
    }

    #[test]
    fn replace_within_refuses_multiple_matches() {
        let mut m = map_with("small small\n");
        let a0 = anchor(&m, 0);
        let err = m.replace_within(a0, a0, "small", "x").unwrap_err();
        assert!(matches!(err, EngineError::MatchCount { count: 2, .. }));
        let err0 = m.replace_within(a0, a0, "nope", "x").unwrap_err();
        assert!(matches!(err0, EngineError::MatchCount { count: 0, .. }));
    }

    #[test]
    fn insert_before_after_mirror_rules() {
        let mut m = map_with("anchor\n");
        let a0 = anchor(&m, 0);
        m.insert(a0, Direction::After, "x").unwrap();
        assert_eq!(m.lines, vec!["anchor", "x"]);
        let a1 = anchor(&m, 1);
        m.insert(a1, Direction::After, "").unwrap(); // one blank line
        assert_eq!(m.lines, vec!["anchor", "x", ""]);
        let a2 = anchor(&m, 2);
        m.insert(a2, Direction::Before, "y").unwrap();
        assert_eq!(m.lines, vec!["anchor", "x", "y", ""]);
    }

    #[test]
    fn insert_after_trailing_break_no_blank() {
        let mut m = map_with("top\n");
        let a0 = anchor(&m, 0);
        m.insert(a0, Direction::After, "x\n").unwrap();
        assert_eq!(m.lines, vec!["top", "x"]); // anchor line directly after x
        m.insert(a0, Direction::After, "z\n\n").unwrap();
        assert_eq!(m.lines, vec!["top", "z", "", "x"]);
    }

    #[test]
    fn copy_then_move_between_maps() {
        let mut src = map_with("s1\ns2\ns3\n");
        let mut dst = map_with("d1\n");
        let (sf, st) = (anchor(&src, 0), anchor(&src, 1));
        let payload = src.take_range(sf, st).unwrap();
        let df = anchor(&dst, 0);
        dst.paste_range(df, &payload, false).unwrap();
        assert_eq!(dst.lines, vec!["d1", "s1", "s2"]);
        // copy keeps source intact
        assert_eq!(src.lines, vec!["s1", "s2", "s3"]);
        // move: remove source range
        src.remove_range(sf, st).unwrap();
        assert_eq!(src.lines, vec!["s3"]);
    }

    #[test]
    fn undo_restores_last_mutation() {
        let mut m = map_with("a\nb\n");
        let a0 = anchor(&m, 0);
        m.replace(a0, a0, "X").unwrap();
        assert_eq!(m.lines, vec!["X", "b"]);
        m.undo().unwrap();
        assert_eq!(m.lines, vec!["a", "b"]);
        assert!(matches!(m.undo(), Err(EngineError::UndoStale)));
    }

    #[test]
    fn stale_anchor_refused() {
        let mut m = map_with("line1\nline2\n");
        let a0 = anchor(&m, 0);
        // simulate external edit without the engine
        m.lines[0] = "tampered".into();
        let err = m.replace(a0, a0, "x").unwrap_err();
        assert!(matches!(err, EngineError::AnchorStale(_)));
    }

    #[test]
    fn split_lines_edge_cases() {
        assert_eq!(split_lines(""), vec![""]);
        assert_eq!(split_lines("a"), vec!["a"]);
        assert_eq!(split_lines("a\n"), vec!["a"]);
        assert_eq!(split_lines("a\nb"), vec!["a", "b"]);
        assert_eq!(split_lines("a\r\nb\r\n"), vec!["a", "b"]);
    }
}
