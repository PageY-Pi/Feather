//! anchor_grep: regex search that serves hits as editable
//! `lineNumber │ anchor│content` rows.
//!
//! Hits come back as rows usable directly by replace/insert/copy/move —
//! no fresh read needed. Files are mapped on demand (bounded by the engine's
//! size guard); long lines (>500 bytes) are shown as a `...` fragment around
//! the hit while the anchor still covers the whole line.

use crate::engine::EngineError;
use crate::store::AnchorStore;
use regex::RegexBuilder;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct GrepOptions {
    pub pattern: String,
    pub path: Option<PathBuf>,
    pub glob: Option<String>,
    pub literal: bool,
    pub ignore_case: bool,
    pub context: usize,
    pub limit: usize,
}

impl Default for GrepOptions {
    fn default() -> Self {
        Self {
            pattern: String::new(),
            path: None,
            glob: None,
            literal: false,
            ignore_case: false,
            context: 0,
            limit: 100,
        }
    }
}

#[derive(Debug, Clone)]
pub struct GrepHit {
    pub path: PathBuf,
    pub line_number: usize,
    pub row: String,
    /// True when the row was cut to a fragment around the hit.
    pub fragment: bool,
}

#[derive(Debug, Clone)]
pub struct GrepOutput {
    pub hits: Vec<GrepHit>,
    /// `=== path ===` section headers interleaved as rows.
    pub sections: Vec<String>,
    pub truncated: bool,
}

/// Directories never searched (dependency/output noise).
const SKIP_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    "dist",
    ".feather",
    "__pycache__",
];

pub fn anchor_grep(
    store: &AnchorStore,
    root: &Path,
    opts: &GrepOptions,
) -> Result<GrepOutput, EngineError> {
    let pattern = if opts.literal {
        regex::escape(&opts.pattern)
    } else {
        opts.pattern.clone()
    };
    let re = RegexBuilder::new(&pattern)
        .case_insensitive(opts.ignore_case)
        .size_limit(10 * 1024 * 1024)
        .build()
        .map_err(|e| EngineError::Io(format!("bad pattern: {e}")))?;
    let glob_re = opts.glob.as_ref().map(|g| glob_to_regex(g));

    let mut hits: Vec<GrepHit> = Vec::new();
    let mut sections: Vec<String> = Vec::new();
    let mut truncated = false;

    let files = collect_files(root, opts.path.as_deref(), glob_re.as_ref())?;
    for file in &files {
        if hits.len() >= opts.limit {
            truncated = true;
            break;
        }
        // map lazily; oversized/binary files are skipped silently here
        let Ok(rows) = store.read(file, None, None) else {
            continue;
        };
        let mut is_hit = vec![false; rows.len()];
        let mut matched = 0usize;
        for (i, row) in rows.iter().enumerate() {
            let content = row.split_once('│').map(|(_, c)| c).unwrap_or(row);
            if re.is_match(content) {
                is_hit[i] = true;
                matched += 1;
                if matched >= opts.limit {
                    truncated = true;
                    break;
                }
            }
        }
        if matched == 0 {
            continue;
        }
        sections.push(format!("=== {} ===", file.display()));
        // expand with context into deduped boolean masks (no O(n²) scans)
        let mut show = vec![false; rows.len()];
        for (i, &hit) in is_hit.iter().enumerate() {
            if hit {
                let lo = i.saturating_sub(opts.context);
                let hi = (i + opts.context).min(rows.len() - 1);
                show[lo..=hi].fill(true);
            }
        }
        for (j, &shown) in show.iter().enumerate() {
            if !shown {
                continue;
            }
            let row = &rows[j];
            let (anchor_str, content) = row.split_once('│').unwrap_or(("", row.as_str()));
            let is_hit = is_hit[j];
            let (rendered, frag) = if content.len() > 500 && is_hit {
                // cut a fragment around the first match; anchor covers the line
                let pos = re.find(content).map(|m| m.start()).unwrap_or(0);
                let start = pos.saturating_sub(120);
                let end = (pos + 360).min(content.len());
                // snap to char boundaries
                let start = floor_char_boundary(content, start);
                let end = floor_char_boundary(content, end);
                let mut f = String::new();
                if start > 0 {
                    f.push_str("...");
                }
                f.push_str(&content[start..end]);
                if end < content.len() {
                    f.push_str("...");
                }
                (f, true)
            } else {
                ((*content).to_string(), false)
            };
            let prefix = if is_hit { "" } else { " " };
            hits.push(GrepHit {
                path: file.clone(),
                line_number: j + 1,
                row: format!("{j} │ {prefix}{anchor_str}│{rendered}"),
                fragment: frag,
            });
        }
    }
    if truncated {
        sections.push("... (match limit reached; refine pattern or raise limit)".into());
    }
    Ok(GrepOutput {
        hits,
        sections,
        truncated,
    })
}

fn floor_char_boundary(s: &str, mut i: usize) -> usize {
    if i >= s.len() {
        return s.len();
    }
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Collect candidate files: an explicit file is used directly; a directory
/// is walked (skipping noise dirs, honoring the optional glob).
fn collect_files(
    root: &Path,
    explicit: Option<&Path>,
    glob: Option<&regex::Regex>,
) -> Result<Vec<PathBuf>, EngineError> {
    let mut out = Vec::new();
    if let Some(p) = explicit {
        if p.is_file() {
            out.push(p.to_path_buf());
            return Ok(out);
        }
        // a directory: walk it below
    }
    let mut stack = vec![explicit.unwrap_or(root).to_path_buf()];
    let mut cap_hit = false;
    while let Some(dir) = stack.pop() {
        if cap_hit {
            break;
        }
        let rd = match std::fs::read_dir(&dir) {
            Ok(rd) => rd,
            Err(_) => continue,
        };
        for entry in rd.flatten() {
            let p = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if p.is_dir() {
                if SKIP_DIRS.contains(&name.as_str()) || name.starts_with('.') {
                    continue;
                }
                stack.push(p);
            } else {
                if let Some(g) = glob {
                    let rel = p
                        .strip_prefix(root)
                        .unwrap_or(&p)
                        .to_string_lossy()
                        .to_string();
                    if !g.is_match(&rel) && !g.is_match(&name) {
                        continue;
                    }
                }
                out.push(p);
                if out.len() > 20_000 {
                    cap_hit = true; // stop the WHOLE walk, not one directory
                    break;
                }
            }
        }
    }
    out.sort();
    Ok(out)
}

/// Translate a simple glob (`*` crosses directories, e.g. `*.ts`) into a
/// regex, as documented: relative to the search root.
fn glob_to_regex(glob: &str) -> regex::Regex {
    let mut pat = String::from("^");
    for c in glob.chars() {
        match c {
            '*' => pat.push_str(".*"),
            '?' => pat.push('.'),
            '.' | '+' | '(' | ')' | '[' | ']' | '{' | '}' | '^' | '$' | '|' | '\\' => {
                pat.push('\\');
                pat.push(c);
            }
            _ => pat.push(c),
        }
    }
    pat.push('$');
    RegexBuilder::new(&pat)
        .case_insensitive(true)
        .build()
        .unwrap_or_else(|_| regex::Regex::new("^$").unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::AnchorStore;

    fn setup() -> (PathBuf, AnchorStore) {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "feather-grep-{}-{}-{}",
            std::process::id(),
            feather_core::message::now_millis(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("a.rs"),
            "fn main() {\n    let small = 1;\n    let big = 2;\n}\n",
        )
        .unwrap();
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(
            root.join("sub/b.ts"),
            "const small = 'x';\nconst other = 'y';\n",
        )
        .unwrap();
        std::fs::write(root.join("skip.log"), "unrelated noise\n").unwrap();
        let store = AnchorStore::new(&root.join(".feather"));
        (root, store)
    }

    #[test]
    fn finds_hits_with_editable_rows() {
        let (root, store) = setup();
        let out = anchor_grep(
            &store,
            &root,
            &GrepOptions {
                pattern: "small".into(),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(out.hits.len(), 2);
        assert!(out.sections.iter().any(|s| s.ends_with("a.rs ===")));
        // row format: `lineNumber │ anchor│content`
        let row = &out.hits[0].row;
        assert!(row.contains(" │ "));
        assert!(row.split_once('│').is_some());
        // the anchor part must parse
        let after_num = row.split_once(" │ ").unwrap().1;
        // anchor chars sit right before the separator and must parse
        assert_eq!(after_num[4..].chars().next(), Some('│'));
        assert!(crate::engine::Anchor::parse(&after_num[..4]).is_some());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn literal_and_case_and_glob() {
        let (root, store) = setup();
        let out = anchor_grep(
            &store,
            &root,
            &GrepOptions {
                pattern: "SMALL".into(),
                ignore_case: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(out.hits.len(), 2);

        let out2 = anchor_grep(
            &store,
            &root,
            &GrepOptions {
                pattern: "small".into(),
                literal: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(out2.hits.len(), 2);

        let out3 = anchor_grep(
            &store,
            &root,
            &GrepOptions {
                pattern: "small".into(),
                glob: Some("*.ts".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(out3.hits.len(), 1);
        assert!(out3.sections.iter().any(|s| s.ends_with("b.ts ===")));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn long_lines_become_fragments() {
        let (root, store) = setup();
        let long = format!("{}NEEDLE{}", "x".repeat(400), "y".repeat(400));
        std::fs::write(root.join("long.txt"), long).unwrap();
        let out = anchor_grep(
            &store,
            &root,
            &GrepOptions {
                pattern: "NEEDLE".into(),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(out.hits.len(), 1);
        assert!(out.hits[0].fragment);
        assert!(out.hits[0].row.contains("..."));
        // anchor still covers the whole line: 4 chars before the │
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn context_rows_are_included_and_deduped() {
        let (root, store) = setup();
        let out = anchor_grep(
            &store,
            &root,
            &GrepOptions {
                pattern: "big".into(),
                context: 1,
                ..Default::default()
            },
        )
        .unwrap();
        // hit on line 3 (+1 context each side): rows 2..4 of a.rs
        assert_eq!(out.hits.len(), 3);
        let nums: Vec<usize> = out.hits.iter().map(|h| h.line_number).collect();
        assert_eq!(nums, vec![2, 3, 4]);
        // only the middle row is a hit: hit rows have no space between the
        // ` │ ` separator and the anchor, context rows carry one padding space
        let hit_row = &out.hits[1].row;
        let ctx_row = &out.hits[0].row;
        let hit_anchor = hit_row.split_once(" │ ").unwrap().1;
        let ctx_anchor = ctx_row.split_once(" │ ").unwrap().1;
        assert!(!hit_anchor.starts_with(' '), "hit row padded: {hit_row}");
        assert!(
            ctx_anchor.starts_with(' '),
            "context row unpadded: {ctx_row}"
        );
        assert!(
            ctx_anchor.trim_start().contains("│"),
            "context anchor parseable"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn grep_hits_are_directly_editable() {
        let (root, store) = setup();
        let f = root.join("a.rs");
        let out = anchor_grep(
            &store,
            &root,
            &GrepOptions {
                pattern: "big".into(),
                path: Some(f.clone()),
                ..Default::default()
            },
        )
        .unwrap();
        let row = &out.hits[0].row;
        // extract anchor: `2 │ abcd│    let big = 2;`
        let after_num = row.split_once(" │ ").unwrap().1;
        let anchor_str = &after_num[..4];
        let a = crate::engine::Anchor::parse(anchor_str).unwrap();
        store.replace_within(&f, a, a, "big", "huge").unwrap();
        assert!(std::fs::read_to_string(&f)
            .unwrap()
            .contains("let huge = 2;"));
        let _ = std::fs::remove_dir_all(&root);
    }
}
