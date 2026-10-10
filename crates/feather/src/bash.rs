//! bash tool — tokio::process, wall-clock timeout, abort-watch aware.
//! Runs actual Bash on every platform (Git Bash/MSYS2 on Windows).

use feather_core::tool::{AgentTool, ToolOutcome};

pub struct BashTool {
    pub timeout: std::time::Duration,
    pub cwd: std::path::PathBuf,
}

impl AgentTool for BashTool {
    fn name(&self) -> &'static str {
        "bash"
    }

    fn description(&self) -> &'static str {
        "Run a Bash command in the project directory and return stdout+stderr. On Windows uses Git Bash/MSYS2 (POSIX paths, pipelines, quoting), not cmd or PowerShell. Each call starts in the project directory; cd does not persist. \
         Params: command (string, required). Use for builds, tests, git, and file inspection. \
         Never modify file contents through the shell (python/sed/awk/heredoc/redirect) — \
         file edits belong to the hashline tools (read → replace/insert/write), which keep \
         anchor-verified diffs and undo."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {"command": {"type": "string"}},
            "required": ["command"]
        })
    }

    fn execute<'a>(
        &'a self,
        args: serde_json::Value,
        abort: tokio::sync::watch::Receiver<bool>,
    ) -> feather_core::tool::BoxFut<'a> {
        Box::pin(async move {
            let command = match args.get("command").and_then(|v| v.as_str()) {
                Some(c) if !c.trim().is_empty() => c.to_string(),
                _ => return ToolOutcome::err("bash: command is required"),
            };

            if *abort.borrow() {
                return ToolOutcome::err("bash: aborted by user");
            }
            if let Err(error)=crate::command_policy::check(&command){return ToolOutcome::err(error);}
            let runtime = match crate::shell::BashRuntime::discover() {
                Ok(shell) => shell,
                Err(e) => return ToolOutcome::err(e),
            };
            let mut cmd = feather_core::process::async_command(&runtime.executable);
            cmd.args(["--noprofile", "--norc", "-c", &command]);
            if let Some(path) = runtime.path {
                cmd.env("PATH", path);
            }
            cmd.env("LANG", "en_US.UTF-8")
                .env("LC_ALL", "en_US.UTF-8")
                .env_remove("BASH_ENV")
                .env_remove("ENV")
                .kill_on_drop(true);
            cmd.current_dir(&self.cwd)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .stdin(std::process::Stdio::null());

            let mut child = match cmd.spawn() {
                Ok(c) => c,
                Err(e) => return ToolOutcome::err(format!("bash: spawn failed: {e}")),
            };

            // Drain both pipes and wait under ONE deadline, including commands
            // whose descendants keep a pipe open after the shell exits.
            let mut abort = abort;
            let mut abort_open = true;
            let pid = child.id();
            let stdout = child.stdout.take();
            let stderr = child.stderr.take();
            let completion = async {
                let (status, stdout, stderr) = tokio::join!(
                    child.wait(),
                    drain_to_string(stdout),
                    drain_to_string(stderr)
                );
                let status = status?;
                let output = if stderr.is_empty() {
                    stdout
                } else if stdout.is_empty() {
                    stderr
                } else {
                    let mut output=stdout;output.push('\n');output.push_str(&stderr);output
                };
                Ok::<_, std::io::Error>((status, output))
            };
            let mut completion = Box::pin(completion);
            let deadline = tokio::time::sleep(self.timeout);
            tokio::pin!(deadline);
            let reason = loop {
                tokio::select! {
                    result=&mut completion=>return match result {
                        Ok((status,text))=>{
                            let text=truncate_owned(text,20_000);
                            ToolOutcome{text:format!("{text}\n[exit {}]",status.code().unwrap_or(-1)),is_error:!status.success(),details:None}
                        },
                        Err(e)=>ToolOutcome::err(format!("bash: wait failed: {e}")),
                    },
                    _=&mut deadline=>break format!("bash: timed out after {}s (command killed)",self.timeout.as_secs_f64()),
                    changed=abort.changed(),if abort_open=>{
                        if changed.is_err(){abort_open=false;}
                        else if *abort.borrow(){break "bash: aborted by user".into();}
                    }
                }
            };
            drop(completion);
            // Killing the shell alone leaves native descendants running on
            // Windows; taskkill /T handles that tree without opening a console.
            #[cfg(windows)]
            if let Some(pid) = pid {
                let mut kill = feather_core::process::async_command("taskkill.exe");
                kill.args(["/PID", &pid.to_string(), "/T", "/F"])
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null());
                let _ =
                    tokio::time::timeout(std::time::Duration::from_secs(3), kill.status()).await;
            }
            #[cfg(not(windows))]
            let _ = pid;
            let _ = child.kill().await;
            let _ = child.wait().await;
            ToolOutcome::err(reason)
        })
    }
}

/// Keep at most 8 MiB but continue draining so full pipes cannot block exit.
const MAX_PIPE_BYTES: usize = 8 * 1024 * 1024;
async fn drain_to_string(pipe: Option<impl tokio::io::AsyncRead + Unpin>) -> String {
    use tokio::io::AsyncReadExt;
    let Some(mut pipe) = pipe else {
        return String::new();
    };
    let mut buf = Vec::new();
    let mut truncated = false;
    let mut chunk = [0u8; 8192];
    loop {
        match pipe.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let keep = n.min(MAX_PIPE_BYTES - buf.len());
                buf.extend_from_slice(&chunk[..keep]);
                truncated |= keep < n;
            }
        }
    }
    // A cap at an arbitrary byte must not turn valid UTF-8 into replacement
    // glyphs or trigger the legacy Windows decoder for the entire buffer.
    if truncated {
        if let Err(e) = std::str::from_utf8(&buf) {
            if e.error_len().is_none() {
                buf.truncate(e.valid_up_to());
            }
        }
    }
    let mut text = crate::shell::decode_output(&buf);
    if truncated {
        text.push_str("\n… (output exceeded 8MB cap, truncated) …");
    }
    truncate_owned(text,20_000)
}

fn truncate_owned(text:String,max_chars:usize)->String {
    if text.chars().count()<=max_chars {text} else {truncate(&text,max_chars)}
}
fn truncate(s: &str, max_chars: usize) -> String {
    let chars=s.chars().count();
    if chars <= max_chars {
        s.to_string()
    } else {
        let head: String = s.chars().take(max_chars / 2).collect();
        let tail: String = s.chars().skip(chars - max_chars / 2).collect();
        format!("{head}\n… (output truncated) …\n{tail}")
    }
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn prohibited_chain_is_not_spawned_and_next_allowed_tool_still_runs() {
        let root=std::env::temp_dir().join(feather_core::provider::gen_id("bash-policy"));std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("source.rs"),"original").unwrap();
        let tool=BashTool{cwd:root.clone(),timeout:std::time::Duration::from_secs(2)};
        let(_tx,rx)=tokio::sync::watch::channel(false);
        let blocked=tool.execute(serde_json::json!({"command":"touch should-not-exist; sed -i 's/original/changed/' source.rs"}),rx.clone()).await;
        assert!(blocked.is_error);assert!(blocked.text.contains("[ILLEGAL_COMMAND]"));
        assert!(!root.join("should-not-exist").exists());assert_eq!(std::fs::read_to_string(root.join("source.rs")).unwrap(),"original");
        let next=tool.execute(serde_json::json!({"command":"printf 'allowed correction'"}),rx).await;
        assert!(!next.is_error);assert!(next.text.contains("allowed correction"));
        std::fs::remove_dir_all(root).unwrap();
    }
    use super::*;

    async fn run(command: &str) -> ToolOutcome {
        let tool = BashTool {
            cwd: std::env::temp_dir(),
            timeout: std::time::Duration::from_secs(15),
        };
        let (_tx, rx) = tokio::sync::watch::channel(false);
        tool.execute(serde_json::json!({"command":command}), rx)
            .await
    }
    #[tokio::test]
    async fn posix_paths_semicolons_loops_redirects_and_pipeline_work() {
        let out=run("cd /; printf 'one\\ntwo\\nthree\\n'; for n in A B; do printf '%s\\n' \"$n\"; done; printf 'quiet' 2>/dev/null; pwd; ls | head -3").await;
        assert!(!out.is_error, "{}", out.text);
        assert!(out.text.contains("one\ntwo\nthree"));
        assert!(out.text.contains("A\nB"));
    }
    #[cfg(windows)]
    #[tokio::test]
    async fn reported_session_path_command_works_without_modifying_history() {
        let home = crate::config::feather_home();
        std::fs::create_dir_all(home.join("sessions")).unwrap();
        let path = home.join("sessions").to_string_lossy().replace('\\', "/");
        let out = run(&format!(
            "cd '{}' && pwd && ls | head -3",
            path.replace('\'', "'\\''")
        ))
        .await;
        assert!(!out.is_error, "{}", out.text);
        assert!(out.text.contains("/sessions"));
    }
    #[tokio::test]
    async fn unicode_stdout_and_stderr_survive_to_tool_card() {
        let out = run("printf '中文工具输出\\n'; printf '错误路径：羽毛\\n' >&2").await;
        assert!(!out.is_error, "{}", out.text);
        assert!(out.text.contains("中文工具输出"));
        assert!(out.text.contains("错误路径：羽毛"));
        assert!(!out.text.contains('\u{fffd}'));
    }
    #[tokio::test]
    async fn exit_status_and_closed_abort_channel_are_honest() {
        let tool = BashTool {
            cwd: std::env::temp_dir(),
            timeout: std::time::Duration::from_secs(10),
        };
        let (tx, rx) = tokio::sync::watch::channel(false);
        drop(tx);
        let out = tool
            .execute(
                serde_json::json!({"command":"sleep 0.1; printf '中文错误\\n' >&2; exit 7"}),
                rx,
            )
            .await;
        assert!(out.is_error);
        assert!(out.text.contains("中文错误"));
        assert!(out.text.contains("[exit 7]"));
    }
    #[tokio::test]
    async fn output_over_cap_is_drained_not_deadlocked() {
        let out = run("head -c 9000000 /dev/zero | tr '\\0' x").await;
        assert!(!out.is_error, "{}", out.text);
        assert!(out.text.contains("8MB cap"));
    }

    #[tokio::test]
    async fn runs_and_captures_output() {
        let t = BashTool {
            timeout: std::time::Duration::from_secs(20),
            cwd: std::env::temp_dir(),
        };
        let (_tx, rx) = tokio::sync::watch::channel(false);
        let out = t
            .execute(serde_json::json!({"command": "echo feather-ok"}), rx)
            .await;
        assert!(!out.is_error, "{}", out.text);
        assert!(out.text.contains("feather-ok"));
    }

    #[tokio::test]
    async fn large_output_does_not_deadlock() {
        // >64KB through the pipe: without concurrent draining, wait() blocks
        // behind a full pipe buffer and only the timeout rescues the call.
        let t = BashTool {
            timeout: std::time::Duration::from_secs(20),
            cwd: std::env::temp_dir(),
        };
        let (_tx, rx) = tokio::sync::watch::channel(false);
        let command = if cfg!(windows) {
            r#"powershell -NoProfile -Command "1..2000 | ForEach-Object { 'x' * 200 }""#.to_string()
        } else {
            r#"head -c 400000 /dev/zero | tr '\0' 'x'"#.to_string()
        };
        let out = t
            .execute(serde_json::json!({ "command": command }), rx)
            .await;
        assert!(
            !out.is_error,
            "large output deadlocked/timed out: {}",
            out.text
        );
        assert!(out.text.contains("exit 0"));
    }

    #[tokio::test]
    async fn times_out() {
        let t = BashTool {
            timeout: std::time::Duration::from_secs(1),
            cwd: std::env::temp_dir(),
        };
        let (_tx, rx) = tokio::sync::watch::channel(false);
        let out = t
            .execute(serde_json::json!({"command": "sleep 30"}), rx)
            .await;
        assert!(out.is_error);
        assert!(out.text.contains("timed out"));
    }
}
