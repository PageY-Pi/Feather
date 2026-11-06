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
         Params: command (string, required), description (string, required), timeout (integer seconds, optional). \
         Set `timeout` from what the command actually needs: a build or a full test suite can \
         take several minutes, while a listing takes seconds. Omitting it uses the configured \
         default, and a command that hits the limit is killed with a message telling you to \
         retry with a larger `timeout` — so say how long you expect to need. \
         Always set `description` to one short phrase (under 60 chars) saying what the command \
         is for, e.g. \"run the gui tests\" or \"check which files changed\". It is shown to the \
         user instead of the raw command, so they can follow along without decoding shell syntax. \
         Never put secrets in it. Use for builds, tests, git, and file inspection. \
         Never modify file contents through the shell (python/sed/awk/heredoc/redirect) — \
         file edits belong to the hashline tools (read → replace/insert/write), which keep \
         anchor-verified diffs and undo."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": {"type": "string"},
                "description": {
                    "type": "string",
                    "description": "Short phrase (under 60 chars) describing what this command does, shown in the UI"
                },
                "timeout": {
                    "type": "integer",
                    "description": "Wall-clock limit for this command, in seconds. Set it from what the command actually needs: a build or test suite may take minutes, a listing takes seconds. Omit only for short commands. Clamped to 1..=3600."
                }
            },
            // `description` is listed as required on purpose. Measured over 573
            // real calls, a prompt-style "recommended" field was omitted every
            // single time: models reliably fill only the required list. Nothing
            // enforces it at runtime (the tool still works when it is absent),
            // so this is a hint about how models actually behave, not a schema
            // contract the caller must satisfy.
            // `timeout` is not "required" for the same measured reason the
            // `description` note above describes, but unlike `description` it
            // is *not* enforced: a call that omits it inherits the configured
            // default rather than failing, because most calls are short and a
            // missing timeout is not a mistake worth a round trip. What matters
            // is that the choice is now the caller's: the old fixed 120s cap
            // could not express "build this, take the time it needs" and killed
            // honest work with no way to ask for more.
            "required": ["command", "description"]
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

            // The intent phrase is a hard requirement, not a nicety. Without it
            // the UI can only echo the raw command, and every card in a session
            // reads as a wall of shell. Refusing the call is the only thing that
            // reliably produces it: prompting for an optional field measured at
            // 0 hits across 573 real calls.
            if let Err(error) = check_intent(&args) {
                return ToolOutcome::err(error);
            }
            // The caller decides how long its command may take; `self.timeout`
            // is only the default for calls that do not say.
            let (timeout, timeout_note) = resolve_timeout(&args, self.timeout);

            if *abort.borrow() {
                return ToolOutcome::err("bash: aborted by user");
            }
            if let Err(error) = crate::command_policy::check(&command) {
                return ToolOutcome::err(error);
            }
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
                    let mut output = stdout;
                    output.push('\n');
                    output.push_str(&stderr);
                    output
                };
                Ok::<_, std::io::Error>((status, output))
            };
            let mut completion = Box::pin(completion);
            let deadline = tokio::time::sleep(timeout);
            tokio::pin!(deadline);
            let reason = loop {
                tokio::select! {
                    result=&mut completion=>return match result {
                        Ok((status,text))=>{
                            let text=truncate_owned(text,20_000);
                            let text = match &timeout_note {
                                Some(note) => format!("{text}\n{note}"),
                                None => text,
                            };
                            ToolOutcome{text:format!("{text}\n[exit {}]",status.code().unwrap_or(-1)),is_error:!status.success(),details:None}
                        },
                        Err(e)=>ToolOutcome::err(format!("bash: wait failed: {e}")),
                    },
                    _=&mut deadline=>break format!("bash: timed out after {}s (command killed); retry with a larger `timeout` if it needs longer",timeout.as_secs_f64()),
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
    truncate_owned(text, 20_000)
}

/// Longest intent phrase accepted. The schema advertises 60, but a phrase that
/// runs a few characters over is not worth failing a command the user already
/// waited for: the limit here only rejects a description that has stopped being
/// "short" and would be elided in the card anyway.
const MAX_INTENT_CHARS: usize = 100;

/// Validate the required `description` argument.
///
/// The message is deliberately explicit about the shape, because the only way
/// this error is ever seen is by the model that has to retry.
fn check_intent(args: &serde_json::Value) -> Result<(), String> {
    let intent = args
        .get("description")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .unwrap_or("");
    if intent.is_empty() {
        return Err(
            "bash: `description` is required and must not be empty. Retry with a short phrase \
             (under 60 characters) saying what the command does, e.g. \
             description: \"run the gui tests\". The command was not run."
                .to_string(),
        );
    }
    let len = intent.chars().count();
    if len > MAX_INTENT_CHARS {
        return Err(format!(
            "bash: `description` is {len} characters; keep it under 60. Retry with a short \
             phrase instead of the command text, e.g. description: \"list changed files\". \
             The command was not run."
        ));
    }
    Ok(())
}

/// Ceiling on a caller-supplied timeout.
///
/// The tool runs inside the GUI process, so an unbounded value would let one
/// command outlive any reasonable turn and hold the run open indefinitely. An
/// hour is long enough for a real build and short enough to be recoverable.
const MAX_TIMEOUT_SECS: u64 = 3600;

/// Resolve the timeout for one call: the caller's `timeout`, else the default.
///
/// Returns the clamped value plus, when clamping or substitution happened, a
/// note to append to the outcome so the caller can see what actually applied
/// instead of assuming its number was honoured verbatim.
fn resolve_timeout(
    args: &serde_json::Value,
    default: std::time::Duration,
) -> (std::time::Duration, Option<String>) {
    let raw = match args.get("timeout") {
        None | Some(serde_json::Value::Null) => return (default, None),
        Some(value) => value,
    };
    // A float is a plausible thing for a model to emit (`"timeout": 300.0`) and
    // is not worth failing over, so accept any non-negative number.
    let seconds = match raw.as_u64() {
        Some(n) => n,
        None => match raw.as_f64() {
            Some(f) if f.is_finite() && f >= 0.0 => f.floor() as u64,
            _ => {
                return (
                    default,
                    Some(format!(
                        "[timeout ignored: expected a whole number of seconds, got {raw}]"
                    )),
                )
            }
        },
    };
    if seconds == 0 {
        return (
            default,
            Some("[timeout ignored: 0s would kill every command; using the default]".to_string()),
        );
    }
    if seconds > MAX_TIMEOUT_SECS {
        return (
            std::time::Duration::from_secs(MAX_TIMEOUT_SECS),
            Some(format!(
                "[timeout clamped to {MAX_TIMEOUT_SECS}s from {seconds}s]"
            )),
        );
    }
    (std::time::Duration::from_secs(seconds), None)
}

fn truncate_owned(text: String, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        text
    } else {
        truncate(&text, max_chars)
    }
}
fn truncate(s: &str, max_chars: usize) -> String {
    let chars = s.chars().count();
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
        let root = std::env::temp_dir().join(feather_core::provider::gen_id("bash-policy"));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("source.rs"), "original").unwrap();
        let tool = BashTool {
            cwd: root.clone(),
            timeout: std::time::Duration::from_secs(2),
        };
        let (_tx, rx) = tokio::sync::watch::channel(false);
        let blocked = tool
            .execute(
                serde_json::json!({
                    "command": "touch should-not-exist; sed -i 's/original/changed/' source.rs",
                    "description": "try a prohibited chain",
                }),
                rx.clone(),
            )
            .await;
        assert!(blocked.is_error);
        assert!(blocked.text.contains("[ILLEGAL_COMMAND]"));
        assert!(!root.join("should-not-exist").exists());
        assert_eq!(
            std::fs::read_to_string(root.join("source.rs")).unwrap(),
            "original"
        );
        let next = tool
            .execute(
                serde_json::json!({
                    "command": "printf 'allowed correction'",
                    "description": "print the allowed marker",
                }),
                rx,
            )
            .await;
        assert!(!next.is_error);
        assert!(next.text.contains("allowed correction"));
        std::fs::remove_dir_all(root).unwrap();
    }
    use super::*;

    /// The intent phrase must stay in `required`.
    ///
    /// It was `optional`/`recommended` first, described in prose in the tool
    /// description. Across 573 recorded calls the model never once supplied it —
    /// it filled only the required list. Moving it into `required` is the only
    /// lever that actually changed model behaviour, so a future tidy-up that
    /// drops it back to optional silently brings the empty headers back.
    #[test]
    fn the_intent_phrase_is_required_not_merely_requested() {
        let schema = BashTool {
            cwd: std::path::PathBuf::from("."),
            timeout: std::time::Duration::from_secs(1),
        }
        .parameters();
        let required: Vec<&str> = schema["required"]
            .as_array()
            .expect("a schema with no required list cannot steer the model")
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert!(required.contains(&"command"));
        assert!(
            required.contains(&"description"),
            "the intent phrase must be required, or models omit it"
        );
        assert!(schema["properties"]["description"].is_object());
    }

    fn tool() -> BashTool {
        BashTool {
            cwd: std::env::temp_dir(),
            timeout: std::time::Duration::from_secs(15),
        }
    }

    async fn run(command: &str) -> ToolOutcome {
        let (_tx, rx) = tokio::sync::watch::channel(false);
        tool()
            .execute(
                serde_json::json!({"command": command, "description": "exercise bash"}),
                rx,
            )
            .await
    }

    /// The intent is enforced, not merely advertised: a call without it must not
    /// run the command. Otherwise the card has only shell text to show.
    #[tokio::test]
    async fn a_call_without_an_intent_is_refused_and_runs_nothing() {
        let (_tx, rx) = tokio::sync::watch::channel(false);
        let out = tool()
            .execute(serde_json::json!({"command": "printf should-not-run"}), rx)
            .await;
        assert!(out.is_error, "a missing description must fail the call");
        assert!(!out.text.contains("should-not-run"), "{}", out.text);
        // The message has to be usable by the model that will retry.
        assert!(out.text.contains("description"), "{}", out.text);

        // Whitespace is not an intent.
        let (_tx, rx) = tokio::sync::watch::channel(false);
        let blank = tool()
            .execute(
                serde_json::json!({"command": "printf should-not-run", "description": "   "}),
                rx,
            )
            .await;
        assert!(blank.is_error, "a blank description must fail the call");
        assert!(!blank.text.contains("should-not-run"), "{}", blank.text);
    }

    /// A description that is no longer a phrase is rejected, but the limit is
    /// looser than the advertised 60 so a marginally long one does not cost a
    /// round trip.
    #[tokio::test]
    async fn an_overlong_intent_is_refused_at_the_documented_boundary() {
        let long = "x".repeat(MAX_INTENT_CHARS + 1);
        let (_tx, rx) = tokio::sync::watch::channel(false);
        let out = tool()
            .execute(
                serde_json::json!({"command": "printf should-not-run", "description": long}),
                rx,
            )
            .await;
        assert!(out.is_error, "{}", out.text);
        assert!(!out.text.contains("should-not-run"), "{}", out.text);

        // Exactly at the limit still runs.
        let at_limit = "y".repeat(MAX_INTENT_CHARS);
        let (_tx, rx) = tokio::sync::watch::channel(false);
        let ok = tool()
            .execute(
                serde_json::json!({"command": "printf ran", "description": at_limit}),
                rx,
            )
            .await;
        assert!(!ok.is_error, "{}", ok.text);
        assert!(ok.text.contains("ran"));
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
                serde_json::json!({
                    "command": "sleep 0.1; printf '中文错误\\n' >&2; exit 7",
                    "description": "emit unicode to both streams",
                }),
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
            .execute(
                serde_json::json!({"command": "echo feather-ok", "description": "echo a marker"}),
                rx,
            )
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
            .execute(
                serde_json::json!({ "command": command, "description": "produce large output" }),
                rx,
            )
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
            .execute(
                serde_json::json!({"command": "sleep 30", "description": "sleep past the timeout"}),
                rx,
            )
            .await;
        assert!(out.is_error);
        assert!(out.text.contains("timed out"));
        // The failure has to say what to do next, or the model just retries the
        // same command and times out again.
        assert!(out.text.contains("larger `timeout`"), "{}", out.text);
    }

    /// The caller's `timeout` decides, not the configured default.
    ///
    /// The default here is deliberately tiny (1s) and the call asks for more:
    /// if the argument were ignored, a 2s sleep would die and the test would
    /// fail. That is the whole point of the change — a long command can now
    /// ask for the time it needs.
    #[tokio::test]
    async fn the_caller_sets_the_timeout_and_may_exceed_the_default() {
        let t = BashTool {
            timeout: std::time::Duration::from_secs(1),
            cwd: std::env::temp_dir(),
        };
        let (_tx, rx) = tokio::sync::watch::channel(false);
        let out = t
            .execute(
                serde_json::json!({
                    "command": "sleep 2; printf finished",
                    "description": "outlast the default timeout",
                    "timeout": 20
                }),
                rx,
            )
            .await;
        assert!(!out.is_error, "{}", out.text);
        assert!(out.text.contains("finished"), "{}", out.text);
        assert!(!out.text.contains("timed out"), "{}", out.text);
    }

    #[test]
    fn the_timeout_argument_is_clamped_and_explained() {
        let default = std::time::Duration::from_secs(120);
        // Omitted: the configured default, and nothing to report.
        assert_eq!(
            resolve_timeout(&serde_json::json!({"command": "x"}), default),
            (default, None)
        );
        // Honoured verbatim.
        assert_eq!(
            resolve_timeout(&serde_json::json!({"timeout": 300}), default),
            (std::time::Duration::from_secs(300), None)
        );
        // A model emitting `300.0` is not worth a failed round trip.
        assert_eq!(
            resolve_timeout(&serde_json::json!({"timeout": 300.0}), default),
            (std::time::Duration::from_secs(300), None)
        );
        // Zero would kill every command before it started, so it falls back.
        let (d, note) = resolve_timeout(&serde_json::json!({"timeout": 0}), default);
        assert_eq!(d, default);
        assert!(note.unwrap().contains("0s"), "zero must explain itself");
        // Above the ceiling is clamped, not refused, and the caller is told.
        let (d, note) = resolve_timeout(&serde_json::json!({"timeout": 999_999}), default);
        assert_eq!(d, std::time::Duration::from_secs(MAX_TIMEOUT_SECS));
        assert!(note.unwrap().contains("clamped"));
        // Garbage falls back rather than failing the command.
        let (d, note) = resolve_timeout(&serde_json::json!({"timeout": "soon"}), default);
        assert_eq!(d, default);
        assert!(note.unwrap().contains("ignored"));
    }

    #[tokio::test]
    async fn a_clamped_timeout_is_reported_in_the_output() {
        let (_tx, rx) = tokio::sync::watch::channel(false);
        let out = tool()
            .execute(
                serde_json::json!({
                    "command": "printf ran",
                    "description": "check the clamp is surfaced",
                    "timeout": 999_999
                }),
                rx,
            )
            .await;
        assert!(!out.is_error, "{}", out.text);
        assert!(out.text.contains("ran"));
        assert!(
            out.text.contains("clamped"),
            "the caller must see the limit that applied: {}",
            out.text
        );
    }
}
