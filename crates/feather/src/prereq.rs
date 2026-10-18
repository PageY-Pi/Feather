//! Host prerequisites Feather cannot ship: a Bash runtime and Git.
//!
//! The Windows installer deliberately bundles neither (size + license), so an
//! install can complete on a machine where the Bash tool and file checkpoints
//! cannot work. Rather than failing in the middle of a run — where the symptom
//! is an opaque tool error — both layers report the gap up front: the installer
//! detects it and offers to install Git for Windows, and this module gives the
//! app the same answer so a user is told what is missing and how to fix it.

use crate::shell::BashRuntime;
use std::path::PathBuf;

/// A dependency that is missing on this host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Missing {
    /// Short name, e.g. `Git Bash`.
    pub name: &'static str,
    /// What stops working, and the exact fix.
    pub detail: &'static str,
}

const BASH_DETAIL: &str = "Bash tools are unavailable: Git Bash/MSYS2 was not found. \
Install Git for Windows (https://git-scm.com/download/win), then reopen Feather, \
or set FEATHER_BASH to an existing bash.exe.";

const GIT_DETAIL: &str = "File checkpoints are unavailable: git was not found. \
Install Git for Windows (https://git-scm.com/download/win), then reopen Feather, \
or set FEATHER_GIT to an existing git.exe.";

/// Everything missing on this machine, in a stable order (Bash first).
pub fn missing() -> Vec<Missing> {
    missing_in(
        BashRuntime::discover()
            .ok()
            .map(|runtime| runtime.executable),
        // Same resolver the checkpoint code uses, so the report never disagrees
        // with what Feather can actually do.
        feather_core::process::git_executable(),
    )
}

/// Testable core: detection inputs are injected so the combination rules are
/// pinned without depending on what the developer machine happens to have.
pub fn missing_in(bash: Option<PathBuf>, git: Option<PathBuf>) -> Vec<Missing> {
    let mut out = Vec::new();
    if bash.is_none() {
        out.push(Missing {
            name: "Git Bash",
            detail: BASH_DETAIL,
        });
    }
    if git.is_none() {
        out.push(Missing {
            name: "git",
            detail: GIT_DETAIL,
        });
    }
    out
}

/// One warning block for a UI that renders text, or `None` when the install is
/// complete. Kept here so the CLI and GUI cannot drift apart.
pub fn warning() -> Option<String> {
    let missing = missing();
    if missing.is_empty() {
        return None;
    }
    let mut text = String::from("Feather needs host tools that this install does not include:");
    for entry in &missing {
        text.push_str("\n\n• ");
        text.push_str(entry.detail);
    }
    Some(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn found() -> Option<PathBuf> {
        Some(Path::new("tool").to_path_buf())
    }

    #[test]
    fn complete_host_reports_nothing() {
        assert_eq!(missing_in(found(), found()), Vec::new());
    }

    #[test]
    fn bare_host_reports_both_in_order() {
        let missing = missing_in(None, None);
        assert_eq!(
            missing.iter().map(|m| m.name).collect::<Vec<_>>(),
            vec!["Git Bash", "git"]
        );
        assert!(missing[0].detail.contains("FEATHER_BASH"));
        assert!(missing[1].detail.contains("FEATHER_GIT"));
    }

    #[test]
    fn one_missing_tool_does_not_hide_the_other() {
        let no_bash = missing_in(None, found());
        assert_eq!(no_bash.len(), 1);
        assert_eq!(no_bash[0].name, "Git Bash");

        let no_git = missing_in(found(), None);
        assert_eq!(no_git.len(), 1);
        assert_eq!(no_git[0].name, "git");
    }

    #[test]
    fn guidance_names_the_fix() {
        // The message is the whole point: it must carry the download link.
        assert!(BASH_DETAIL.contains("https://git-scm.com/download/win"));
        assert!(GIT_DETAIL.contains("https://git-scm.com/download/win"));
    }

    #[test]
    fn warning_mentions_everymissing_tool() {
        // `warning()` reads the real host, so only its shape is asserted here:
        // when anything is missing the text must name it.
        if let Some(text) = warning() {
            let missing = missing();
            assert!(!missing.is_empty());
            for entry in missing {
                assert!(text.contains(entry.name) || text.contains(entry.detail));
            }
            assert!(text.contains("git-scm.com"));
        }
    }
}
