//! AgentTool implementations for the hashline surface.
//!
//! Tool schemas mirror the pi-hashline-edit-pro contract: edits are
//! addressed by 5-char anchors, never line numbers; errors refuse with the
//! current rows attached so the model can retry without a fresh read.

use crate::engine::{Anchor, Direction, EngineError};
use crate::grep::{anchor_grep, GrepOptions};
use crate::store::AnchorStore;
use feather_core::tool::{AgentTool, ToolOutcome};
use serde_json::Value;
use std::sync::Arc;

fn get_str(args: &Value, key: &str) -> Option<String> {
    args.get(key).and_then(|v| v.as_str()).map(str::to_string)
}

fn get_usize(args: &Value, key: &str) -> Option<usize> {
    args.get(key).and_then(|v| v.as_u64()).map(|v| v as usize)
}

fn get_bool(args: &Value, key: &str) -> Option<bool> {
    args.get(key).and_then(|v| v.as_bool())
}

/// Parse an anchor from the first of two spellings that is present.
///
/// `replace` historically used `remove_from`/`remove_to` while its partial-edit
/// sibling used `replace_from`/`replace_to`. Models mixed them up constantly —
/// 1685 real calls sent the other tool's keys — so the merged tool accepts both
/// rather than rejecting a call whose intent is unambiguous.
fn parse_anchor_alias(v: &Value, primary: &str, alias: &str) -> Result<Anchor, ToolOutcome> {
    if v.get(primary).and_then(|x| x.as_str()).is_some() {
        return parse_anchor(v, primary);
    }
    if v.get(alias).and_then(|x| x.as_str()).is_none() {
        return Err(ToolOutcome::err(format!(
            "missing argument: {primary} (or {alias})\n\
             `replace` is the only edit tool; give the first and last anchor of the range."
        )));
    }
    parse_anchor(v, alias)
}
fn parse_anchor(v: &Value, key: &str) -> Result<Anchor, ToolOutcome> {
    let s = v
        .get(key)
        .and_then(|x| x.as_str())
        .ok_or_else(|| ToolOutcome::err(format!("missing string argument: {key}")))?;
    Anchor::parse(s).ok_or_else(|| {
        // A 4-letter string is the shape of a pre-upgrade anchor; name that
        // explicitly and point at the fix, instead of implying nonsense input.
        if s.len() == 4 {
            ToolOutcome::err(format!(
                "[E_ANCHOR_FORMAT] '{s}' is a 4-letter anchor; anchors are now 5 lowercase \
                 letters — re-read the file to get current anchors"
            ))
        } else {
            ToolOutcome::err(format!(
                "[E_ANCHOR_FORMAT] '{s}' is not a 5-lowercase-letter anchor; read the file \
                 (or anchor_grep) to get anchors"
            ))
        }
    })
}

/// Prefix for the first output line that names the edited file.
///
/// The GUI rebuilds tool cards from the persisted transcript, where the
/// structured `details` side-channel does not survive. Keeping the path in the
/// text itself lets a reloaded card name the file exactly like a live one.
/// The prefix is explicit so parsing is a prefix match, never a guess about
/// whether a line "looks like a path".
const FILE_HEADER: &str = "file: ";

/// Name the edited file in the first output line, then the diff body.
fn with_file_header(path: &std::path::Path, body: String) -> String {
    format!("{FILE_HEADER}{}\n{body}", path.display())
}

/// A successful edit outcome that also reports which file it touched.
///
/// `details` is a structured side-channel the GUI reads to label a tool card
/// with the real path — the anchor arguments alone (`remove_from ab3cd`) say
/// nothing a user can recognize. It is never sent to the model, so the same
/// path is also written into the text for transcript reloads.
fn edited(path: &std::path::Path, diff: &[crate::engine::DiffRow]) -> ToolOutcome {
    let mut out = ToolOutcome::ok(with_file_header(path, render_diff(diff)));
    out.details = Some(serde_json::json!({ "path": path.display().to_string() }));
    out
}

fn render_diff(rows: &[crate::engine::DiffRow]) -> String {
    let mut out = String::new();
    for r in rows {
        out.push_str(&r.render());
        out.push('\n');
    }
    out
}

fn engine_out(res: Result<String, EngineError>) -> ToolOutcome {
    match res {
        Ok(text) => ToolOutcome::ok(text),
        Err(e) => ToolOutcome::err(e.to_string()),
    }
}

// ---------------------------------------------------------------------------
// read
// ---------------------------------------------------------------------------
pub struct ReadTool {
    pub store: Arc<AnchorStore>,
}

impl AgentTool for ReadTool {
    fn name(&self) -> &'static str {
        "read"
    }

    fn description(&self) -> &'static str {
        // Kept short on purpose: this string is billed on every request. The
        // shell-vs-anchor editing policy already lives in the system prompt, so
        // repeating it here paid for it twice; the legacy 4-letter-anchor note
        // only matters for sessions predating the alphabet change and is now a
        // clause rather than a sentence.
        "Read a text file as anchor rows `anchor│content` (5 lowercase letters). A 4-letter anchor predates this build — re-read. \
Line numbers are not shown and are not accepted by edit tools; edit by anchor. \
Page with offset (1-indexed) and limit; when output says truncated, continue from the hinted offset/limit. \
Images, binary, directories, and UTF-16/32 text are rejected; an empty file returns one empty row you can replace to seed content."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "File to read" },
                "offset": { "type": "integer", "description": "1-indexed line to start from" },
                "limit": { "type": "integer", "description": "Max lines to read" }
            },
            "required": ["path"]
        })
    }

    fn execute<'a>(
        &'a self,
        args: Value,
        _abort: tokio::sync::watch::Receiver<bool>,
    ) -> feather_core::tool::BoxFut<'a> {
        Box::pin(async move {
            let Some(path) = get_str(&args, "path") else {
                return ToolOutcome::err("missing argument: path");
            };
            let offset = get_usize(&args, "offset");
            let limit = get_usize(&args, "limit");
            let p = crate::store::norm_key(std::path::Path::new(&path));
            if p.is_dir() {
                return ToolOutcome::err("[E_IS_DIR] path is a directory, not a file");
            }
            match self.store.read(&p, offset, limit) {
                Ok(rows) => {
                    let total = self.store.total_lines(&p).unwrap_or(0);
                    let start = offset.unwrap_or(1);
                    let end = start + rows.len().saturating_sub(1);
                    let mut text = rows.join("\n");
                    if !text.is_empty() {
                        text.push('\n');
                    }
                    if end < total {
                        text.push_str(&format!(
                            "(truncated; {end}/{total} lines served — continue with offset {})\n",
                            end + 1
                        ));
                    }
                    ToolOutcome::ok(text)
                }
                Err(e) => ToolOutcome::err(e.to_string()),
            }
        })
    }
}

// ---------------------------------------------------------------------------
// replace
// ---------------------------------------------------------------------------
pub struct ReplaceTool {
    pub store: Arc<AnchorStore>,
}

impl AgentTool for ReplaceTool {
    fn name(&self) -> &'static str {
        "replace"
    }

    fn description(&self) -> &'static str {
        "Replace lines in a text file by anchor, either wholesale or in part. \
Whole lines: `remove_from`/`remove_to` are the 5-char anchors of the first and last line to replace, and `replacement_lines` is the exact new text — \"\" deletes the range. \
Part of a line: pass `replace_old` (copied exactly from the served row, must occur once in the range) and `replace_new`; every character you do not name is preserved, so nothing needs retyping. \
Target lines by anchor, never by content or line number. \
A trailing line break sets the last line's ending instead of adding a blank line. \
Path resolution is anchor-only. Same-file calls in one message batch share one undo."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "remove_from": { "type": "string", "description": "5-char anchor of the FIRST line to replace (alias: replace_from)" },
                "remove_to": { "type": "string", "description": "5-char anchor of the LAST line to replace (alias: replace_to)" },
                "replacement_lines": { "type": "string", "description": "Whole-line mode: exact new text for the range; \"\" deletes it" },
                "replace_old": { "type": "string", "description": "Partial mode: text to replace, copied exactly from the served row; must occur once in the range" },
                "replace_new": { "type": "string", "description": "Partial mode: text to put in its place; \"\" deletes it" },
                "replace_from": { "type": "string", "description": "Alias for remove_from" },
                "replace_to": { "type": "string", "description": "Alias for remove_to" }
            }
        })
    }

    fn execute<'a>(
        &'a self,
        args: Value,
        _abort: tokio::sync::watch::Receiver<bool>,
    ) -> feather_core::tool::BoxFut<'a> {
        Box::pin(async move {
            // Anchors arrive as either `remove_*` or `replace_*`. Requiring one
            // exact spelling is what made models reach for `replace` and get a
            // missing-argument error, so both are accepted.
            let from = match parse_anchor_alias(&args, "remove_from", "replace_from") {
                Ok(a) => a,
                Err(e) => return e,
            };
            let to = match parse_anchor_alias(&args, "remove_to", "replace_to") {
                Ok(a) => a,
                Err(e) => return e,
            };
            // Which mode this is depends on which content field is present. The
            // old code defaulted `replacement_lines` to "" and so deleted the
            // range when the field was simply omitted — a silent destructive
            // default. Omitting both is now an error rather than a deletion.
            let old = get_str(&args, "replace_old");
            let new = get_str(&args, "replace_new");
            match (old, new) {
                (Some(old), new) => {
                    let new = new.unwrap_or_default();
                    match self.store.replace_within_anchors(from, to, &old, &new) {
                        Ok((path, diff)) => edited(&path, &diff),
                        Err(e) => engine_out(Err(e)),
                    }
                }
                (None, Some(_)) => ToolOutcome::err(
                    "replace_new needs replace_old: partial replacement requires the text to \
                     replace. To rewrite whole lines instead, pass replacement_lines.",
                ),
                (None, None) => {
                    let Some(replacement) = get_str(&args, "replacement_lines") else {
                        return ToolOutcome::err(
                            "missing argument: replacement_lines or replace_old\n\
                             Whole lines: remove_from, remove_to, replacement_lines (\"\" deletes).\n\
                             Part of a line: remove_from, remove_to, replace_old, replace_new.",
                        );
                    };
                    match self.store.replace_anchors(from, to, &replacement) {
                        Ok((path, diff)) => edited(&path, &diff),
                        Err(e) => engine_out(Err(e)),
                    }
                }
            }
        })
    }
}

// ---------------------------------------------------------------------------
// insert
// ---------------------------------------------------------------------------
pub struct InsertTool {
    pub store: Arc<AnchorStore>,
}

impl AgentTool for InsertTool {
    fn name(&self) -> &'static str {
        "insert"
    }

    fn description(&self) -> &'static str {
        "Insert text after or before one existing line, addressed by a 5-char anchor. \
The anchor line is preserved: `lines` go after it with direction \"after\" or before it with direction \"before\". \
\"\" inserts one blank line; a trailing line break sets the last line's ending instead of adding a blank line. \
Never include the anchor line. Compare both edges of the inserted block in the post-edit diff against the request."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "anchor": { "type": "string", "description": "5-char anchor of the line to insert next to" },
                "direction": { "type": "string", "enum": ["after", "before"] },
                "lines": { "type": "string", "description": "Exact text to insert; empty string inserts one blank line" }
            },
            "required": ["anchor", "direction", "lines"]
        })
    }

    fn execute<'a>(
        &'a self,
        args: Value,
        _abort: tokio::sync::watch::Receiver<bool>,
    ) -> feather_core::tool::BoxFut<'a> {
        Box::pin(async move {
            let anchor = match parse_anchor(&args, "anchor") {
                Ok(a) => a,
                Err(e) => return e,
            };
            let direction = match get_str(&args, "direction").as_deref() {
                Some("after") => Direction::After,
                Some("before") => Direction::Before,
                _ => return ToolOutcome::err("direction must be \"after\" or \"before\""),
            };
            let lines = get_str(&args, "lines").unwrap_or_default();
            match self.store.insert_anchors(anchor, direction, &lines) {
                Ok((path, diff)) => edited(&path, &diff),
                Err(e) => engine_out(Err(e)),
            }
        })
    }
}

// ---------------------------------------------------------------------------
// copy
// ---------------------------------------------------------------------------
pub struct CopyTool {
    pub store: Arc<AnchorStore>,
}

impl AgentTool for CopyTool {
    fn name(&self) -> &'static str {
        "copy"
    }

    fn description(&self) -> &'static str {
        "Copy a range of lines to another position, targeted by 5-character anchors. \
`source_from`/`source_to` mark the first and last line to copy and `insert_after` the destination line after which the copy goes; anchors may belong to different files. \
The source lines stay in place, and the copied lines are written exactly as the source file holds them."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "source_from": { "type": "string" },
                "source_to": { "type": "string" },
                "insert_after": { "type": "string" }
            },
            "required": ["source_from", "source_to", "insert_after"]
        })
    }

    fn execute<'a>(
        &'a self,
        args: Value,
        _abort: tokio::sync::watch::Receiver<bool>,
    ) -> feather_core::tool::BoxFut<'a> {
        Box::pin(async move {
            let sf = match parse_anchor(&args, "source_from") {
                Ok(a) => a,
                Err(e) => return e,
            };
            let st = match parse_anchor(&args, "source_to") {
                Ok(a) => a,
                Err(e) => return e,
            };
            let ia = match parse_anchor(&args, "insert_after") {
                Ok(a) => a,
                Err(e) => return e,
            };
            match self.store.copy_anchors(sf, st, ia) {
                Ok((_src, dst, diff)) => edited(&dst, &diff),
                Err(e) => engine_out(Err(e)),
            }
        })
    }
}

// ---------------------------------------------------------------------------
// move
// ---------------------------------------------------------------------------
pub struct MoveTool {
    pub store: Arc<AnchorStore>,
}

impl AgentTool for MoveTool {
    fn name(&self) -> &'static str {
        "move"
    }

    fn description(&self) -> &'static str {
        "Move a range of lines to another position, targeted by 5-character anchors. \
`source_from`/`source_to` mark the range and `insert_after` the destination line after which the range goes; anchors may belong to different files. \
`insert_after` must sit outside the source range when both anchors share a file; moving a range to where it already sits reports no change."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "source_from": { "type": "string" },
                "source_to": { "type": "string" },
                "insert_after": { "type": "string" }
            },
            "required": ["source_from", "source_to", "insert_after"]
        })
    }

    fn execute<'a>(
        &'a self,
        args: Value,
        _abort: tokio::sync::watch::Receiver<bool>,
    ) -> feather_core::tool::BoxFut<'a> {
        Box::pin(async move {
            let sf = match parse_anchor(&args, "source_from") {
                Ok(a) => a,
                Err(e) => return e,
            };
            let st = match parse_anchor(&args, "source_to") {
                Ok(a) => a,
                Err(e) => return e,
            };
            let ia = match parse_anchor(&args, "insert_after") {
                Ok(a) => a,
                Err(e) => return e,
            };
            match self.store.move_anchors(sf, st, ia) {
                Ok((_src, dst, removed, added)) => {
                    let mut out = ToolOutcome::ok(with_file_header(
                        &dst,
                        format!("{}{}", render_diff(&removed), render_diff(&added)),
                    ));
                    out.details = Some(serde_json::json!({ "path": dst.display().to_string() }));
                    out
                }
                Err(e) => engine_out(Err(e)),
            }
        })
    }
}

// ---------------------------------------------------------------------------
// anchor_grep
// ---------------------------------------------------------------------------
pub struct AnchorGrepTool {
    pub store: Arc<AnchorStore>,
    pub root: std::path::PathBuf,
}

impl AgentTool for AnchorGrepTool {
    fn name(&self) -> &'static str {
        "anchor_grep"
    }

    fn description(&self) -> &'static str {
        "Search text files; hits come back as `lineNumber │ anchor│content` rows, editable with replace/insert/copy/move without a new read; the `=== path ===` header and line numbers locate the match. \
Searches respect common ignore dirs and skip binary files silently. A match over 500 bytes is shown as a `...` fragment around the hit, but its anchor still covers the whole line. \
When cut by match limit, raise `limit`; when cut by rows/bytes, refine `pattern` or narrow `path`."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "Regex (or literal text with literal=true)" },
                "path": { "type": "string", "description": "Directory or file to search (default: workspace root)" },
                "glob": { "type": "string", "description": "Filter files by glob; `*` crosses directories, e.g. `*.ts`" },
                "literal": { "type": "boolean", "description": "Treat pattern as literal text" },
                "ignoreCase": { "type": "boolean" },
                "context": { "type": "integer", "description": "Context lines around each match" },
                "limit": { "type": "integer", "description": "Max matched lines (default 100)" }
            },
            "required": ["pattern"]
        })
    }

    fn execute<'a>(
        &'a self,
        args: Value,
        _abort: tokio::sync::watch::Receiver<bool>,
    ) -> feather_core::tool::BoxFut<'a> {
        Box::pin(async move {
            let pattern = match get_str(&args, "pattern") {
                Some(p) => p,
                None => return ToolOutcome::err("missing argument: pattern"),
            };
            let opts = GrepOptions {
                pattern,
                path: get_str(&args, "path").map(std::path::PathBuf::from),
                glob: get_str(&args, "glob"),
                literal: get_bool(&args, "literal").unwrap_or(false),
                ignore_case: get_bool(&args, "ignoreCase").unwrap_or(false),
                context: get_usize(&args, "context").unwrap_or(0),
                limit: get_usize(&args, "limit").unwrap_or(100).clamp(1, 1000),
            };
            let root = opts.path.as_deref().unwrap_or(self.root.as_path());
            match anchor_grep(&self.store, root, &opts) {
                Ok(out) => {
                    if out.hits.is_empty() {
                        return ToolOutcome::ok("no matches");
                    }
                    let mut text = String::new();
                    // interleave section headers with their hits
                    for (i, hit) in out.hits.iter().enumerate() {
                        let is_new_section = i == 0 || out.hits[i - 1].path != hit.path;
                        if is_new_section {
                            text.push_str(&format!("=== {} ===\n", hit.path.display()));
                        }
                        text.push_str(&hit.row);
                        text.push('\n');
                    }
                    if out.truncated {
                        text.push_str(
                            "... results truncated; refine pattern, narrow path, or raise limit\n",
                        );
                    }
                    ToolOutcome::ok(text)
                }
                Err(e) => ToolOutcome::err(e.to_string()),
            }
        })
    }
}

// ---------------------------------------------------------------------------
// undo_last_change
// ---------------------------------------------------------------------------
pub struct UndoTool {
    pub store: Arc<AnchorStore>,
}

impl AgentTool for UndoTool {
    fn name(&self) -> &'static str {
        "undo_last_change"
    }

    fn description(&self) -> &'static str {
        "Undo the last replace, insert, copy, or move on a file, restoring the previous content. \
Use it when an edit removed or changed the wrong lines. If the file changed since that edit, the undo is refused with [E_UNDO_STALE] and nothing is modified: verify the current state with read and stop instead of forcing the undo."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "File to undo" }
            },
            "required": ["path"]
        })
    }

    fn execute<'a>(
        &'a self,
        args: Value,
        _abort: tokio::sync::watch::Receiver<bool>,
    ) -> feather_core::tool::BoxFut<'a> {
        Box::pin(async move {
            let Some(path) = get_str(&args, "path") else {
                return ToolOutcome::err("missing argument: path");
            };
            let undo_path = crate::store::norm_key(std::path::Path::new(&path));
            match self.store.undo(&undo_path) {
                Ok(diff) => edited(&undo_path, &diff),
                Err(e) => engine_out(Err(e)),
            }
        })
    }
}

// ---------------------------------------------------------------------------
// write
// ---------------------------------------------------------------------------
pub struct WriteTool {
    pub store: Arc<AnchorStore>,
}

impl AgentTool for WriteTool {
    fn name(&self) -> &'static str {
        "write"
    }

    fn description(&self) -> &'static str {
        "Create a new file or fully rewrite an existing one. The file is written and re-anchored; the served rows are fresh anchors for subsequent edit tools. \
Prefer hashline edit tools for changing part of an existing file."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string" },
                "content": { "type": "string" }
            },
            "required": ["path", "content"]
        })
    }

    fn execute<'a>(
        &'a self,
        args: Value,
        _abort: tokio::sync::watch::Receiver<bool>,
    ) -> feather_core::tool::BoxFut<'a> {
        Box::pin(async move {
            let Some(path) = get_str(&args, "path") else {
                return ToolOutcome::err("missing argument: path");
            };
            let Some(content) = get_str(&args, "content") else {
                return ToolOutcome::err("missing argument: content");
            };
            let write_path = crate::store::norm_key(std::path::Path::new(&path));
            match self.store.write(&write_path, &content) {
                Ok(rows) => {
                    let mut out = ToolOutcome::ok(with_file_header(&write_path, rows.join("\n")));
                    out.details =
                        Some(serde_json::json!({ "path": write_path.display().to_string() }));
                    out
                }
                Err(e) => ToolOutcome::err(e.to_string()),
            }
        })
    }
}

/// Assemble the hashline tool set.
pub fn hashline_tools(
    store: Arc<AnchorStore>,
    root: std::path::PathBuf,
) -> Vec<Arc<dyn AgentTool>> {
    vec![
        Arc::new(ReadTool {
            store: store.clone(),
        }),
        Arc::new(WriteTool {
            store: store.clone(),
        }),
        Arc::new(ReplaceTool {
            store: store.clone(),
        }),
        Arc::new(InsertTool {
            store: store.clone(),
        }),
        Arc::new(CopyTool {
            store: store.clone(),
        }),
        Arc::new(MoveTool {
            store: store.clone(),
        }),
        Arc::new(AnchorGrepTool {
            store: store.clone(),
            root,
        }),
        Arc::new(UndoTool { store }),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use feather_core::tool::ToolRegistry;
    use std::sync::Arc;

    fn tmp(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!(
            "feather-tools-{tag}-{}-{}",
            std::process::id(),
            feather_core::message::now_millis()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[tokio::test]
    async fn tool_roundtrip_via_registry() {
        let dir = tmp("reg");
        let f = dir.join("t.txt");
        std::fs::write(&f, "old line\n").unwrap();
        let store = Arc::new(AnchorStore::new(&dir.join(".feather")));
        let mut reg = ToolRegistry::new();
        for t in hashline_tools(store, dir.clone()) {
            reg.add(t);
        }
        let (_, abort) = tokio::sync::watch::channel(false);

        // read
        let out = reg
            .execute_call(
                &feather_core::message::ToolCall {
                    id: "1".into(),
                    name: "read".into(),
                    arguments: serde_json::json!({"path": f.to_string_lossy()}),
                },
                abort.clone(),
            )
            .await;
        assert!(!out.is_error, "{out:?}");
        let row = out.text.lines().next().unwrap().to_string();
        let anchor = row.split_once('│').unwrap().0.to_string();

        // replace by anchor (no path argument!)
        let out = reg
            .execute_call(
                &feather_core::message::ToolCall {
                    id: "2".into(),
                    name: "replace".into(),
                    arguments: serde_json::json!({
                        "remove_from": anchor,
                        "remove_to": anchor,
                        "replacement_lines": "new line"
                    }),
                },
                abort.clone(),
            )
            .await;
        assert!(!out.is_error, "{out:?}");
        assert!(std::fs::read_to_string(&f).unwrap().contains("new line"));
        // The anchor arguments carry no path, so the GUI can only name the file
        // if the outcome reports it. Losing this is invisible in the diff text.
        let reported = out
            .details
            .as_ref()
            .and_then(|d| d.get("path"))
            .and_then(|p| p.as_str())
            .expect("edit outcomes must report the edited path");
        assert!(reported.ends_with("t.txt"), "reported path was {reported}");
        // The same path must also be in the text: `details` is not persisted,
        // so a reloaded card reads the file from line one instead.
        let first = out.text.lines().next().unwrap();
        assert!(
            first.starts_with("file: ") && first.ends_with("t.txt"),
            "first output line was {first:?}"
        );

        // grep finds the new content and serves an editable row
        let out = reg
            .execute_call(
                &feather_core::message::ToolCall {
                    id: "3".into(),
                    name: "anchor_grep".into(),
                    arguments: serde_json::json!({"pattern": "new line", "path": dir.to_string_lossy()}),
                },
                abort.clone(),
            )
            .await;
        assert!(!out.is_error, "{out:?}");
        assert!(out.text.contains("│"), "grep output was: {}", out.text);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn descriptions_state_the_real_anchor_shape() {
        // The tool description is the model's only spec for anchor syntax. It
        // said "4-character, letters-only, case-sensitive" for a while after the
        // alphabet changed; keep it pinned to the constants instead.
        let dir = tmp("desc");
        let store = Arc::new(AnchorStore::new(&dir.join(".feather")));
        let len = crate::engine::ANCHOR_LEN.to_string();
        for t in hashline_tools(store, dir.clone()) {
            let d = t.description();
            assert!(
                !d.contains("4-char") && !d.contains("case-sensitive"),
                "{} description still describes 4-char case-sensitive anchors: {d}",
                t.name()
            );
            // Descriptions that state a width must state the current one; ones
            // that stay quiet about width (write) need no assertion.
            if d.contains("-char") || d.contains("-character") {
                assert!(
                    d.contains(&format!("{len}-char")) || d.contains(&format!("{len}-character")),
                    "{} description states the wrong anchor width: {d}",
                    t.name()
                );
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn four_letter_anchor_is_refused_with_a_reread_hint() {
        // An anchor left over from a pre-upgrade conversation must produce a
        // pointed refusal, not a generic format error and never a guess.
        let dir = tmp("oldanchor");
        let f = dir.join("t.txt");
        std::fs::write(&f, "alpha\n").unwrap();
        let store = Arc::new(AnchorStore::new(&dir.join(".feather")));
        let mut reg = ToolRegistry::new();
        for t in hashline_tools(store, dir.clone()) {
            reg.add(t);
        }
        let (_, abort) = tokio::sync::watch::channel(false);
        let out = reg
            .execute_call(
                &feather_core::message::ToolCall {
                    id: "1".into(),
                    name: "replace".into(),
                    arguments: serde_json::json!({
                        "remove_from": "abcd",
                        "remove_to": "abcd",
                        "replacement_lines": "x"
                    }),
                },
                abort.clone(),
            )
            .await;
        assert!(out.is_error, "a 4-letter anchor must be refused");
        assert!(
            out.text.contains("[E_ANCHOR_FORMAT]"),
            "expected E_ANCHOR_FORMAT, got: {}",
            out.text
        );
        assert!(
            out.text.contains("re-read"),
            "the refusal must tell the model to re-read: {}",
            out.text
        );
        // the file is untouched
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "alpha\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // --- the merged replace: both shapes, and the destructive default ---

    async fn two_line_file(
        tag: &str,
    ) -> (std::path::PathBuf, std::path::PathBuf, Arc<AnchorStore>) {
        let dir = tmp(tag);
        let f = dir.join("t.txt");
        std::fs::write(&f, "alpha line\nbeta line\n").unwrap();
        let store = Arc::new(AnchorStore::new(&dir.join(".feather")));
        (dir, f, store)
    }

    /// Read the file and return (anchor of the line containing `needle`, text).
    async fn anchor_for(store: &Arc<AnchorStore>, f: &std::path::Path, needle: &str) -> String {
        let mut reg = ToolRegistry::new();
        for t in hashline_tools(store.clone(), f.parent().unwrap().to_path_buf()) {
            reg.add(t);
        }
        let (_, abort) = tokio::sync::watch::channel(false);
        let out = reg
            .execute_call(
                &feather_core::message::ToolCall {
                    id: "r".into(),
                    name: "read".into(),
                    arguments: serde_json::json!({"path": f.to_string_lossy()}),
                },
                abort,
            )
            .await;
        assert!(!out.is_error, "{}", out.text);
        for line in out.text.lines() {
            let Some((anchor, rest)) = line.split_once('│') else {
                continue;
            };
            if rest.contains(needle) {
                return anchor.trim().to_string();
            }
        }
        panic!("no line containing {needle:?} in: {}", out.text);
    }

    #[tokio::test]
    async fn replace_accepts_the_replace_style_anchor_names() {
        // The bug this covers: 1685 real calls passed `replace_from`/`replace_to`
        // to a tool that only accepted `remove_from`/`remove_to`.
        let (dir, f, store) = two_line_file("alias").await;
        let a = anchor_for(&store, &f, "beta line").await;
        let mut reg = ToolRegistry::new();
        for t in hashline_tools(store, dir.clone()) {
            reg.add(t);
        }
        let (_, abort) = tokio::sync::watch::channel(false);
        let out = reg
            .execute_call(
                &feather_core::message::ToolCall {
                    id: "1".into(),
                    name: "replace".into(),
                    arguments: serde_json::json!({
                        "remove_from": a, "remove_to": a, "replacement_lines": "beta changed"
                    }),
                },
                abort,
            )
            .await;
        assert!(!out.is_error, "{}", out.text);
        assert_eq!(
            std::fs::read_to_string(&f).unwrap(),
            "alpha line\nbeta changed\n"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn replace_partial_mode_preserves_the_rest_of_the_line() {
        let (dir, f, store) = two_line_file("partial").await;
        let a = anchor_for(&store, &f, "beta line").await;
        let mut reg = ToolRegistry::new();
        for t in hashline_tools(store, dir.clone()) {
            reg.add(t);
        }
        let (_, abort) = tokio::sync::watch::channel(false);
        let out = reg
            .execute_call(
                &feather_core::message::ToolCall {
                    id: "1".into(),
                    name: "replace".into(),
                    arguments: serde_json::json!({
                        "replace_from": a, "replace_to": a,
                        "replace_old": "beta", "replace_new": "BETA"
                    }),
                },
                abort,
            )
            .await;
        assert!(!out.is_error, "{}", out.text);
        // Only the named text changed; " line" survived.
        assert_eq!(
            std::fs::read_to_string(&f).unwrap(),
            "alpha line\nBETA line\n"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn omitting_the_content_field_does_not_delete_the_range() {
        // The old code did `get_str(...).unwrap_or_default()`, so a call that
        // simply forgot `replacement_lines` silently deleted the lines. Losing
        // content must require an explicit "".
        let (dir, f, store) = two_line_file("nodelete").await;
        let a = anchor_for(&store, &f, "beta line").await;
        let mut reg = ToolRegistry::new();
        for t in hashline_tools(store, dir.clone()) {
            reg.add(t);
        }
        let (_, abort) = tokio::sync::watch::channel(false);
        let out = reg
            .execute_call(
                &feather_core::message::ToolCall {
                    id: "1".into(),
                    name: "replace".into(),
                    arguments: serde_json::json!({"remove_from": a, "remove_to": a}),
                },
                abort,
            )
            .await;
        assert!(
            out.is_error,
            "omitting content must be refused: {}",
            out.text
        );
        assert!(out.text.contains("missing argument"), "{}", out.text);
        // And the file is intact.
        assert_eq!(
            std::fs::read_to_string(&f).unwrap(),
            "alpha line\nbeta line\n"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn an_explicit_empty_replacement_still_deletes() {
        // The guard above must not remove the ability to delete on purpose.
        let (dir, f, store) = two_line_file("empty").await;
        let a = anchor_for(&store, &f, "beta line").await;
        let mut reg = ToolRegistry::new();
        for t in hashline_tools(store, dir.clone()) {
            reg.add(t);
        }
        let (_, abort) = tokio::sync::watch::channel(false);
        let out = reg
            .execute_call(
                &feather_core::message::ToolCall {
                    id: "1".into(),
                    name: "replace".into(),
                    arguments: serde_json::json!({
                        "remove_from": a, "remove_to": a, "replacement_lines": ""
                    }),
                },
                abort,
            )
            .await;
        assert!(!out.is_error, "{}", out.text);
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "alpha line\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn both_anchor_spellings_point_at_one_tool() {
        // The whole point of the merge: there is no `replace_within` to pick by
        // mistake, and asking for it must fail as an unknown tool rather than
        // silently landing somewhere unexpected.
        let (dir, _f, store) = two_line_file("onetool").await;
        let mut reg = ToolRegistry::new();
        for t in hashline_tools(store, dir.clone()) {
            reg.add(t);
        }
        let names = reg.names();
        assert!(names.contains(&"replace"), "{names:?}");
        assert!(!names.contains(&"replace_within"), "{names:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
