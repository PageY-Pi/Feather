//! AgentTool implementations for the hashline surface.
//!
//! Tool schemas mirror the pi-hashline-edit-pro contract: edits are
//! addressed by 4-char anchors, never line numbers; errors refuse with the
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

fn parse_anchor(v: &Value, key: &str) -> Result<Anchor, ToolOutcome> {
    let s = v
        .get(key)
        .and_then(|x| x.as_str())
        .ok_or_else(|| ToolOutcome::err(format!("missing string argument: {key}")))?;
    Anchor::parse(s).ok_or_else(|| {
        ToolOutcome::err(format!("[E_ANCHOR_FORMAT] '{s}' is not a 4-letter anchor"))
    })
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
        "Read a text file as anchor rows `anchor│content` (4-character, letters-only, case-sensitive). \
Prefer the hashline tools for anything that touches files: read, replace, replace_within, insert, copy, move, write — the shell (python/sed) is for running code, never for editing files. \
Line numbers are not shown and are not accepted by edit tools — edit by anchor. \
Page long files with offset (1-indexed) and limit; when the output says truncated, continue with the hinted offset/limit. \
Images, binary, directory, and UTF-16/UTF-32 text are rejected; an empty file returns one empty row you can replace to seed content."
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
        "Replace a range of lines (or a single line) in a text file by anchor. \
`remove_from` and `remove_to` are the 4-character anchors of the first and last line to remove, and `replacement_lines` is one string with the exact replacement text: \"\" deletes the range. \
Target lines by anchor, never by content or line number. \
A trailing line break sets the last line's ending instead of adding a blank line. \
Path resolution is anchor-only. Same-file calls in one message batch share one undo."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "remove_from": { "type": "string", "description": "4-char anchor of the FIRST line to remove" },
                "remove_to": { "type": "string", "description": "4-char anchor of the LAST line to remove" },
                "replacement_lines": { "type": "string", "description": "Exact replacement text; \"\" deletes" }
            },
            "required": ["remove_from", "remove_to", "replacement_lines"]
        })
    }

    fn execute<'a>(
        &'a self,
        args: Value,
        _abort: tokio::sync::watch::Receiver<bool>,
    ) -> feather_core::tool::BoxFut<'a> {
        Box::pin(async move {
            let from = match parse_anchor(&args, "remove_from") {
                Ok(a) => a,
                Err(e) => return e,
            };
            let to = match parse_anchor(&args, "remove_to") {
                Ok(a) => a,
                Err(e) => return e,
            };
            let replacement = get_str(&args, "replacement_lines").unwrap_or_default();
            match self.store.replace_anchors(from, to, &replacement) {
                Ok((_path, diff)) => ToolOutcome::ok(render_diff(&diff)),
                Err(e) => engine_out(Err(e)),
            }
        })
    }
}

// ---------------------------------------------------------------------------
// replace_within
// ---------------------------------------------------------------------------
pub struct ReplaceWithinTool {
    pub store: Arc<AnchorStore>,
}

impl AgentTool for ReplaceWithinTool {
    fn name(&self) -> &'static str {
        "replace_within"
    }

    fn description(&self) -> &'static str {
        "Replace part of a line (or a range of lines) without retyping the rest. \
`replace_old` is matched exactly once inside `replace_from`/`replace_to` and replaced by `replace_new`; every character the request does not name is preserved. \
`replace_old` must be copied exactly from the served row and occur once in the range; a missing or repeated match is refused and returns the current rows."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "replace_from": { "type": "string", "description": "4-char anchor of the FIRST line searched" },
                "replace_to": { "type": "string", "description": "4-char anchor of the LAST line searched" },
                "replace_old": { "type": "string" },
                "replace_new": { "type": "string" }
            },
            "required": ["replace_from", "replace_to", "replace_old", "replace_new"]
        })
    }

    fn execute<'a>(
        &'a self,
        args: Value,
        _abort: tokio::sync::watch::Receiver<bool>,
    ) -> feather_core::tool::BoxFut<'a> {
        Box::pin(async move {
            let from = match parse_anchor(&args, "replace_from") {
                Ok(a) => a,
                Err(e) => return e,
            };
            let to = match parse_anchor(&args, "replace_to") {
                Ok(a) => a,
                Err(e) => return e,
            };
            let old = match get_str(&args, "replace_old") {
                Some(s) => s,
                None => return ToolOutcome::err("missing argument: replace_old"),
            };
            let new = get_str(&args, "replace_new").unwrap_or_default();
            match self.store.replace_within_anchors(from, to, &old, &new) {
                Ok((_path, diff)) => ToolOutcome::ok(render_diff(&diff)),
                Err(e) => engine_out(Err(e)),
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
        "Insert text after or before one existing line, addressed by a 4-char anchor. \
The anchor line is preserved: `lines` go after it with direction \"after\" or before it with direction \"before\". \
\"\" inserts one blank line; a trailing line break sets the last line's ending instead of adding a blank line. \
Never include the anchor line. Compare both edges of the inserted block in the post-edit diff against the request."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "anchor": { "type": "string", "description": "4-char anchor of the line to insert next to" },
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
                Ok((_path, diff)) => ToolOutcome::ok(render_diff(&diff)),
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
        "Copy a range of lines to another position, targeted by 4-character anchors. \
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
                Ok((_src, _dst, diff)) => ToolOutcome::ok(render_diff(&diff)),
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
        "Move a range of lines to another position, targeted by 4-character anchors. \
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
                Ok((_src, _dst, removed, added)) => {
                    ToolOutcome::ok(format!("{}{}", render_diff(&removed), render_diff(&added)))
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
        "Undo the last replace, replace_within, insert, copy, or move on a file, restoring the previous content. \
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
                Ok(diff) => ToolOutcome::ok(render_diff(&diff)),
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
                Ok(rows) => ToolOutcome::ok(rows.join("\n")),
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
        Arc::new(ReplaceWithinTool {
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
}
