//! Directory enumeration must never block an input/caret frame.
use eframe::egui;
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
#[derive(Default)]
pub struct Listing {
    pub dirs: Vec<String>,
    pub files: Vec<String>,
}
#[derive(Default)]
struct State {
    value: Arc<Listing>,
    updated: Option<Instant>,
    pending: bool,
}
const IGNORE: [&str; 11] = [
    ".git",
    "node_modules",
    "target",
    "dist",
    "build",
    "__pycache__",
    ".next",
    ".venv",
    "venv",
    "coverage",
    ".feather",
];
pub fn listing(ctx: &egui::Context, path: &Path) -> Arc<Listing> {
    let id = egui::Id::new(("directory-cache", path));
    let cell = ctx.data_mut(|d| d.get_temp_mut_or_default::<Arc<Mutex<State>>>(id).clone());
    let mut state = cell.lock().unwrap_or_else(|e| e.into_inner());
    let refresh = !state.pending
        && state
            .updated
            .is_none_or(|at| at.elapsed() > Duration::from_secs(2));
    let value = state.value.clone();
    if refresh {
        state.pending = true;
        let cell = cell.clone();
        let ctx = ctx.clone();
        let path = path.to_path_buf();
        std::thread::spawn(move || {
            let value = Arc::new(read_listing(&path));
            let mut state = cell.lock().unwrap_or_else(|e| e.into_inner());
            state.value = value;
            state.updated = Some(Instant::now());
            state.pending = false;
            drop(state);
            ctx.request_repaint_after(Duration::from_millis(33));
        });
    }
    value
}
fn read_listing(path: &PathBuf) -> Listing {
    let mut value = Listing::default();
    if let Ok(read) = std::fs::read_dir(path) {
        for entry in read.flatten().take(10000) {
            let name = entry.file_name().to_string_lossy().to_string();
            if IGNORE.contains(&name.as_str()) {
                continue;
            }
            if entry.file_type().is_ok_and(|ty| ty.is_dir()) {
                value.dirs.push(name);
            } else {
                value.files.push(name);
            }
        }
    }
    value.dirs.sort();
    value.files.sort();
    value.dirs.truncate(200);
    value.files.truncate(300);
    value
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn async_directory_cache_excludes_build_trees() {
        let root = std::env::temp_dir().join(feather_core::provider::gen_id("workspace-cache"));
        std::fs::create_dir_all(root.join("target")).unwrap();
        std::fs::write(root.join("file.rs"), "").unwrap();
        let ctx = egui::Context::default();
        let started = Instant::now();
        loop {
            let value = listing(&ctx, &root);
            if !value.files.is_empty() {
                assert_eq!(value.files, vec!["file.rs"]);
                assert!(!value.dirs.contains(&"target".into()));
                break;
            }
            assert!(started.elapsed() < Duration::from_secs(3));
            std::thread::sleep(Duration::from_millis(2));
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}
