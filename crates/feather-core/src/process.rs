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
