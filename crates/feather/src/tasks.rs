//! pi-task-tracker — compiled-in task management (no plugins, no scripts).
//!
//! Tasks persist as JSONL in the session dir; four tools surface them to
//! the model: task_create / task_update / task_list / task_get.

use feather_core::tool::{AgentTool, ToolOutcome};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Mutex;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub title: String,
    pub description: String,
    /// pending | in_progress | done | cancelled
    pub status: String,
    pub created_ms: u64,
    pub updated_ms: u64,
}

pub struct TaskStore {
    path: PathBuf,
    tasks: Mutex<Vec<Task>>,
}

fn now() -> u64 {
    feather_core::message::now_millis()
}

impl TaskStore {
    pub fn open(dir: &std::path::Path) -> Self {
        Self::try_open(dir).expect("open valid task store")
    }
    pub fn try_open(dir: &std::path::Path) -> Result<Self, String> {
        let path = dir.join("tasks.redb");
        let store = feather_core::records::Records::open(&path)?;
        let tasks = match store.get::<Vec<Task>>("tasks")? {
            Some(tasks) => tasks,
            None => {
                let legacy = dir.join("tasks.jsonl");
                let tasks: Vec<Task> = if legacy.exists() {
                    std::fs::read_to_string(&legacy)
                        .map_err(|e| e.to_string())?
                        .lines()
                        .filter(|line| !line.trim().is_empty())
                        .map(|line| serde_json::from_str(line).map_err(|e| e.to_string()))
                        .collect::<Result<_, _>>()?
                } else {
                    Vec::new()
                };
                store.put("tasks", &tasks)?;
                let verified = store
                    .get::<Vec<Task>>("tasks")?
                    .ok_or("task migration missing")?;
                if serde_json::to_vec(&verified).map_err(|e| e.to_string())?
                    != serde_json::to_vec(&tasks).map_err(|e| e.to_string())?
                {
                    return Err("task migration mismatch".into());
                }
                if legacy.exists() {
                    std::fs::rename(&legacy, dir.join("tasks.jsonl.legacy"))
                        .map_err(|e| e.to_string())?;
                }
                tasks
            }
        };
        Ok(Self {
            path,
            tasks: Mutex::new(tasks),
        })
    }

    fn persist(&self, tasks: &[Task]) {
        feather_core::records::Records::open(&self.path)
            .and_then(|store| store.put("tasks", &tasks))
            .expect("persist task data");
    }

    pub fn create(&self, title: &str, description: &str) -> Task {
        let mut tasks = self.tasks.lock().unwrap_or_else(|e| e.into_inner());
        let t = Task {
            id: format!("t{:03}", tasks.len() + 1),
            title: title.to_string(),
            description: description.to_string(),
            status: "pending".into(),
            created_ms: now(),
            updated_ms: now(),
        };
        tasks.push(t.clone());
        self.persist(&tasks);
        t
    }

    pub fn update(&self, id: &str, status: Option<&str>, title: Option<&str>) -> Option<Task> {
        let mut tasks = self.tasks.lock().unwrap_or_else(|e| e.into_inner());
        let t = tasks.iter_mut().find(|t| t.id == id)?;
        if let Some(s) = status {
            t.status = s.to_string();
        }
        if let Some(ti) = title {
            t.title = ti.to_string();
        }
        t.updated_ms = now();
        let out = t.clone();
        self.persist(&tasks);
        Some(out)
    }

    pub fn list(&self) -> Vec<Task> {
        self.tasks.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn get(&self, id: &str) -> Option<Task> {
        self.tasks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .find(|t| t.id == id)
            .cloned()
    }

    /// Counted in place — the status bar calls this every frame; cloning the
    /// whole task list per frame would be the hot cost.
    pub fn open_count(&self) -> usize {
        self.tasks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter(|t| t.status == "pending" || t.status == "in_progress")
            .count()
    }

    pub fn summary_line(&self) -> String {
        let tasks = self.list();
        if tasks.is_empty() {
            return "no tasks".into();
        }
        tasks
            .iter()
            .map(|t| format!("{} [{}] {}", t.id, t.status, t.title))
            .collect::<Vec<_>>()
            .join(" · ")
    }
    /// pi-task-tracker-style working-directory trackers, minus the git part
    /// (feather never requires explicit git):
    /// - `TODO.md` lists open tasks only (`- [ ]`); completing a task removes
    ///   its entry — completion bookkeeping stays in tasks.jsonl.
    /// - `README.md` gets a normalized `## Overview` + `## Project Structure`
    ///   scaffold; files the agent touched (from session tool calls) are
    ///   appended to the structure block, deduped.
    pub fn sync_workdir_docs(&self, root: &std::path::Path, touched_files: &[String]) {
        let open: Vec<Task> = self
            .list()
            .into_iter()
            .filter(|t| t.status == "pending" || t.status == "in_progress")
            .collect();
        let mut todo = String::from(
            "# TODO\n\nManaged by feather task tools — done tasks leave this file.\n\n",
        );
        if open.is_empty() {
            todo.push_str("_No open tasks._\n");
        }
        for t in &open {
            let mark = if t.status == "in_progress" {
                " _(in progress)_"
            } else {
                ""
            };
            todo.push_str(&format!(
                "- [ ] **{}**{} — {}\n",
                t.title, mark, t.description
            ));
        }
        let _ = std::fs::write(root.join("TODO.md"), todo);

        let readme = root.join("README.md");
        let name = root
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("project");
        let mut content = std::fs::read_to_string(&readme).unwrap_or_else(|_| {
            format!(
                "# {name}\n\n## Overview\n\nTODO: Add project description.\n\n## Project Structure\n\n```\n{name}/\n```\n"
            )
        });
        if !content.contains("## Overview") {
            content = format!(
                "{}\n\n## Overview\n\nTODO: Add project description.\n",
                content.trim_end()
            );
        }
        if !content.contains("## Project Structure") {
            content = format!(
                "{}\n\n## Project Structure\n\n```\n```\n",
                content.trim_end()
            );
        }
        if !touched_files.is_empty() {
            content = append_structure_entries(&content, touched_files);
        }
        let _ = std::fs::write(&readme, content);
    }
}

/// Insert `├── path` entries at the top of the fenced block under
/// `## Project Structure`, skipping entries already present.
fn append_structure_entries(content: &str, touched: &[String]) -> String {
    const MARKER: &str = "## Project Structure";
    let Some(start) = content.find(MARKER) else {
        return content.to_string();
    };
    let Some(open_rel) = content[start..].find("```") else {
        return content.to_string();
    };
    let open = start + open_rel + 3;
    let Some(close_rel) = content[open..].find("```") else {
        return content.to_string();
    };
    let close = open + close_rel;
    let mut lines: Vec<String> = content[open..close]
        .lines()
        .map(str::trim_end)
        .map(str::to_string)
        .collect();
    for f in touched.iter().rev() {
        let normalized = f.replace('\\', "/");
        let path = std::path::Path::new(&normalized);
        if path.is_absolute()
            || normalized.contains(':')
            || normalized
                .split('/')
                .any(|part| matches!(part, ".." | "target" | ".git" | ".feather" | "node_modules"))
        {
            continue;
        }
        let entry = format!("├── {normalized}");
        if !lines.contains(&entry) {
            lines.insert(0, entry);
        }
    }
    let mut out = String::with_capacity(content.len() + touched.len() * 16);
    out.push_str(&content[..open]);
    for l in &lines {
        out.push_str(l);
        out.push('\n');
    }
    out.push_str(&content[close..]);
    out
}
// ---------------------------------------------------------------------------
// Tools (same dyn-compatible shape as the hashline tools)
// ---------------------------------------------------------------------------

macro_rules! json_tool {
    ($name:expr, $desc:expr, $params:expr) => {
        fn name(&self) -> &'static str {
            $name
        }
        fn description(&self) -> &'static str {
            $desc
        }
        fn parameters(&self) -> serde_json::Value {
            serde_json::from_str($params).unwrap()
        }
    };
}

fn get_str<'a>(args: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(|v| v.as_str())
}

pub struct TaskCreateTool(pub std::sync::Arc<TaskStore>);

impl AgentTool for TaskCreateTool {
    json_tool!(
        "task_create",
        "Create a tracked task. Params: title (string, required), description (string).",
        r#"{"type":"object","properties":{"title":{"type":"string"},"description":{"type":"string"}},"required":["title"]}"#
    );
    fn execute<'a>(
        &'a self,
        args: serde_json::Value,
        _abort: tokio::sync::watch::Receiver<bool>,
    ) -> feather_core::tool::BoxFut<'a> {
        Box::pin(async move {
            let title = get_str(&args, "title").unwrap_or("").trim().to_string();
            if title.is_empty() {
                return ToolOutcome::err("task_create: title is required");
            }
            let desc = get_str(&args, "description").unwrap_or("");
            let t = self.0.create(&title, desc);
            ToolOutcome::ok(format!("created {} [{}] {}", t.id, t.status, t.title))
        })
    }
}

pub struct TaskUpdateTool(pub std::sync::Arc<TaskStore>);

impl AgentTool for TaskUpdateTool {
    json_tool!(
        "task_update",
        "Update task status or title. Params: id (string, required), status (pending|in_progress|done|cancelled), title (string).",
        r#"{"type":"object","properties":{"id":{"type":"string"},"status":{"type":"string","enum":["pending","in_progress","done","cancelled"]},"title":{"type":"string"}},"required":["id"]}"#
    );
    fn execute<'a>(
        &'a self,
        args: serde_json::Value,
        _abort: tokio::sync::watch::Receiver<bool>,
    ) -> feather_core::tool::BoxFut<'a> {
        Box::pin(async move {
            let id = get_str(&args, "id").unwrap_or("").to_string();
            let status = get_str(&args, "status");
            let title = get_str(&args, "title");
            if status.is_none() && title.is_none() {
                return ToolOutcome::err("task_update: provide status and/or title");
            }
            match self.0.update(&id, status, title) {
                Some(t) => ToolOutcome::ok(format!("{} [{}] {}", t.id, t.status, t.title)),
                None => ToolOutcome::err(format!("task_update: no task {id}")),
            }
        })
    }
}

pub struct TaskListTool(pub std::sync::Arc<TaskStore>);

impl AgentTool for TaskListTool {
    json_tool!(
        "task_list",
        "List all tracked tasks with ids and statuses.",
        r#"{"type":"object","properties":{}}"#
    );
    fn execute<'a>(
        &'a self,
        _args: serde_json::Value,
        _abort: tokio::sync::watch::Receiver<bool>,
    ) -> feather_core::tool::BoxFut<'a> {
        Box::pin(async move {
            let tasks = self.0.list();
            if tasks.is_empty() {
                return ToolOutcome::ok("no tasks");
            }
            let out = tasks
                .iter()
                .map(|t| format!("{} [{}] {} — {}", t.id, t.status, t.title, t.description))
                .collect::<Vec<_>>()
                .join("\n");
            ToolOutcome::ok(out)
        })
    }
}

pub struct TaskGetTool(pub std::sync::Arc<TaskStore>);

impl AgentTool for TaskGetTool {
    json_tool!(
        "task_get",
        "Get one task by id. Params: id (string, required).",
        r#"{"type":"object","properties":{"id":{"type":"string"}},"required":["id"]}"#
    );
    fn execute<'a>(
        &'a self,
        args: serde_json::Value,
        _abort: tokio::sync::watch::Receiver<bool>,
    ) -> feather_core::tool::BoxFut<'a> {
        Box::pin(async move {
            let id = get_str(&args, "id").unwrap_or("").to_string();
            match self.0.get(&id) {
                Some(t) => ToolOutcome::ok(serde_json::to_string_pretty(&t).unwrap_or_default()),
                None => ToolOutcome::err(format!("task_get: no task {id}")),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn structure_skips_build_outputs_and_outside_paths() {
        let content = "## Project Structure\n\n```\n```\n";
        let paths = [
            "target/debug/test.exe",
            "../outside",
            ".git/index",
            "C:/outside",
            "src/main.rs",
        ];
        let result = append_structure_entries(content, &paths.map(str::to_string));
        assert!(result.contains("├── src/main.rs"));
        assert!(
            !result.contains("outside") && !result.contains("target/") && !result.contains(".git/")
        );
    }

    fn temp(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "feather-tasks-{tag}-{}-{}",
            std::process::id(),
            now()
        ))
    }

    #[test]
    fn create_update_list_roundtrip() {
        let dir = temp("t");
        std::fs::create_dir_all(&dir).unwrap();
        let store = TaskStore::open(&dir);
        let t = store.create("refactor engine", "split splice paths");
        assert_eq!(t.status, "pending");
        store.update(&t.id, Some("in_progress"), None).unwrap();
        assert_eq!(store.open_count(), 1);
        // persistence: reopen from disk
        let store2 = TaskStore::open(&dir);
        assert_eq!(store2.list().len(), 1);
        assert_eq!(store2.list()[0].status, "in_progress");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tools_execute_through_registry() {
        let dir = temp("r");
        std::fs::create_dir_all(&dir).unwrap();
        let store = std::sync::Arc::new(TaskStore::open(&dir));
        let mut reg = feather_core::tool::ToolRegistry::new();
        reg.add(std::sync::Arc::new(TaskCreateTool(store.clone())));
        reg.add(std::sync::Arc::new(TaskUpdateTool(store.clone())));
        reg.add(std::sync::Arc::new(TaskListTool(store.clone())));
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let (atx, arx) = tokio::sync::watch::channel(false);
        rt.block_on(async move {
            let out = reg
                .execute_call(
                    &feather_core::message::ToolCall {
                        id: "1".into(),
                        name: "task_create".into(),
                        arguments: serde_json::json!({"title": "ship feather"}),
                    },
                    arx.clone(),
                )
                .await;
            assert!(!out.is_error);
            let out = reg
                .execute_call(
                    &feather_core::message::ToolCall {
                        id: "2".into(),
                        name: "task_list".into(),
                        arguments: serde_json::json!({}),
                    },
                    atx.subscribe(),
                )
                .await;
            assert!(out.text.contains("ship feather"));
        });
        let _ = std::fs::remove_dir_all(&dir);
    }
    #[test]
    fn workdir_docs_mirror_tasks_and_files() {
        let dir = temp("docs");
        let root = dir.join("proj");
        std::fs::create_dir_all(&root).unwrap();
        let store = TaskStore::open(&dir);
        let t1 = store.create("wire up snapshots", "e2e test");
        let t2 = store.create("audit gaps", "");
        store.update(&t2.id, Some("in_progress"), None).unwrap();

        store.sync_workdir_docs(&root, &["src/lib.rs".into(), "src\\main.rs".into()]);
        let todo = std::fs::read_to_string(root.join("TODO.md")).unwrap();
        assert!(todo.contains("- [ ] **wire up snapshots** — e2e test"));
        assert!(todo.contains("_(in progress)_"));

        // README scaffold normalized on first sync
        let readme = std::fs::read_to_string(root.join("README.md")).unwrap();
        assert!(readme.contains("## Overview"));
        assert!(readme.contains("## Project Structure"));
        assert!(readme.contains("├── src/lib.rs"), "{readme}");
        assert!(
            readme.contains("├── src/main.rs"),
            "backslash normalized: {readme}"
        );

        // completing a task removes its TODO entry; README dedupes on resync
        store.update(&t1.id, Some("done"), None).unwrap();
        store.sync_workdir_docs(&root, &["src/lib.rs".into()]);
        let todo = std::fs::read_to_string(root.join("TODO.md")).unwrap();
        assert!(!todo.contains("wire up snapshots"), "{todo}");
        let readme = std::fs::read_to_string(root.join("README.md")).unwrap();
        assert_eq!(
            readme.matches("├── src/lib.rs").count(),
            1,
            "deduped: {readme}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
