//! Background processes must never steal focus or flash a console over the GUI.
//! This does not detach children: pipes, exit status, waiting and cancellation
//! retain their normal behavior. Flags are applied before every spawn on Windows.
use std::ffi::OsStr;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

pub fn command(program: impl AsRef<OsStr>) -> std::process::Command {
    #[allow(unused_mut)]
    let mut cmd = std::process::Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

pub fn async_command(program: impl AsRef<OsStr>) -> tokio::process::Command {
    #[allow(unused_mut)]
    let mut cmd = tokio::process::Command::new(program);
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);
    cmd
}

/// Resolve Git without trusting PATH alone. Git for Windows can be installed
/// with "use Git from Git Bash only" (no PATH entry), and a process started
/// before the installer refreshed PATH never sees a fresh install — both make
/// `Command::new("git")` fail even though git is present. Checkpoints must work
/// in that state or an install looks broken.
pub fn git_command() -> std::process::Command {
    command(git_program())
}

pub fn git_program() -> std::ffi::OsString {
    git_executable().map_or_else(|| std::ffi::OsString::from("git"), Into::into)
}

pub fn git_executable() -> Option<std::path::PathBuf> {
    let exe = if cfg!(windows) { "git.exe" } else { "git" };
    if let Some(value) = std::env::var_os("FEATHER_GIT") {
        let path = std::path::PathBuf::from(value);
        if path.is_file() {
            return Some(path);
        }
    }
    if let Some(found) = find_on_path(exe) {
        return Some(found);
    }
    #[cfg(windows)]
    for root in git_roots() {
        for relative in ["cmd/git.exe", "bin/git.exe"] {
            let candidate = root.join(relative);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

fn find_on_path(exe: &str) -> Option<std::path::PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .filter(|dir| !dir.as_os_str().is_empty())
        .map(|dir| dir.join(exe))
        .find(|candidate| candidate.is_file())
}

/// Install roots a Git for Windows setup can leave behind, including the
/// per-user default. `FEATHER_BASH` is honored first so an explicit Bash
/// choice also supplies its git.
#[cfg(windows)]
fn git_roots() -> Vec<std::path::PathBuf> {
    let mut roots = Vec::new();
    if let Some(bash) = std::env::var_os("FEATHER_BASH") {
        let bash = std::path::PathBuf::from(bash);
        // <root>/bin/bash.exe or <root>/usr/bin/bash.exe -> <root>
        if let Some(root) = bash.parent().and_then(|dir| {
            if dir.file_name().is_some_and(|n| n == "bin") {
                dir.parent()
            } else {
                dir.parent().and_then(|p| p.parent())
            }
        }) {
            roots.push(root.to_path_buf());
        }
    }
    for var in ["ProgramW6432", "ProgramFiles", "ProgramFiles(x86)"] {
        if let Some(dir) = std::env::var_os(var) {
            roots.push(std::path::PathBuf::from(dir).join("Git"));
        }
    }
    if let Some(dir) = std::env::var_os("LOCALAPPDATA") {
        roots.push(std::path::PathBuf::from(dir).join("Programs/Git"));
    }
    roots
}

#[cfg(test)]
mod tests {
    #[cfg(windows)]
    const CONSOLE_PROBE: &str = "Add-Type -TypeDefinition 'using System; using System.Runtime.InteropServices; public class ConsoleProbe { [DllImport(\"kernel32.dll\")] public static extern IntPtr GetConsoleWindow(); }'; [ConsoleProbe]::GetConsoleWindow().ToInt64()";

    #[cfg(windows)]
    #[test]
    fn sync_child_has_no_console_window() {
        let output = super::command("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command", CONSOLE_PROBE])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "0");
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn async_child_has_no_console_window() {
        let output = super::async_command("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command", CONSOLE_PROBE])
            .output()
            .await
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "0");
    }

    #[test]
    fn background_commands_preserve_output_and_exit_status() {
        let mut cmd = if cfg!(windows) {
            let mut cmd = super::command("cmd");
            cmd.args(["/C", "echo feather-background-ok & exit /b 7"]);
            cmd
        } else {
            let mut cmd = super::command("sh");
            cmd.args(["-c", "echo feather-background-ok; exit 7"]);
            cmd
        };
        let output = cmd.output().unwrap();
        assert_eq!(output.status.code(), Some(7));
        assert!(String::from_utf8_lossy(&output.stdout).contains("feather-background-ok"));
    }
}
