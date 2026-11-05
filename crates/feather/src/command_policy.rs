//! Conservative preflight for clearly disallowed shell edits/destructive git.
//! This is not a Bash sandbox: unknown programs can have side effects. The gate
//! blocks known violations before spawning, and returns a normal tool error so
//! the model can correct it instead of aborting the agent.
//!
//! It also refuses commands that would terminate Feather itself. The agent runs
//! *inside* the GUI process, so an unguarded `taskkill /IM feather-gui.exe` or
//! `kill <own pid>` would take out the runtime mid-turn with no chance to save
//! state — the same failure mode as pulling the power cord.
pub fn check(command: &str) -> Result<(), String> {
    check_depth(command, 0)
}
fn illegal(reason: &str) -> Result<(), String> {
    Err(format!("[ILLEGAL_COMMAND] Blocked before execution: {reason}. Use the anchor file tools or an allowed inspection command. This is a recoverable tool error; continue with a safe alternative."))
}

/// Process names that ARE Feather's own runtime. Killing any of these from a
/// tool call tears down the agent that issued the call.
const SELF_NAMES: &[&str] = &["feather-gui", "feather"];

fn self_pid() -> u32 {
    std::process::id()
}

/// Whether `name` (already lowercase, extension stripped) is Feather itself.
fn is_self_name(name: &str) -> bool {
    SELF_NAMES.contains(&name)
}

/// Refuse terminations aimed at Feather's own processes.
///
/// Covers the shapes that actually reach a kill:
///   taskkill /IM feather-gui.exe        taskkill /PID <own pid>
///   Stop-Process -Name feather-gui      kill -9 <own pid>
///   pkill feather-gui                   killall feather
///
/// A `--` separator, an explicit different PID, or a differently named target
/// is left alone so deliberate process management still works.
fn check_self_kill(program: &str, args: &[String]) -> Result<(), String> {
    let target = |value: &str| {
        let name = value
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or("")
            .trim_end_matches(".exe")
            .to_ascii_lowercase();
        is_self_name(&name)
    };
    let own_pid = self_pid().to_string();
    match program {
        "taskkill" => {
            for (n, arg) in args.iter().enumerate() {
                let value = args.get(n + 1).map(String::as_str).unwrap_or("");
                match arg.to_ascii_lowercase().as_str() {
                    // /IM feather-gui.exe : match by image name.
                    "/im" | "-im" if target(value) => {
                        return illegal("taskkill would terminate Feather's own process");
                    }
                    // /PID <n> : match by PID, including our own.
                    "/pid" | "-pid" if value == own_pid => {
                        return illegal("taskkill would terminate Feather's own process");
                    }
                    _ => {}
                }
            }
        }
        // `kill <pid>` / `kill -9 <pid>`. Only the single-argument form is
        // matched; named targets are handled by pkill/killall below.
        "kill" => {
            for arg in args.iter().filter(|a| !a.starts_with('-')) {
                if arg == &own_pid {
                    return illegal("kill would terminate Feather's own process");
                }
            }
        }
        "pkill" | "killall" => {
            for arg in args.iter().filter(|a| !a.starts_with('-')) {
                if target(arg) {
                    return illegal("this would terminate Feather's own process");
                }
            }
        }
        "stop-process" => {
            for (n, arg) in args.iter().enumerate() {
                let value = args.get(n + 1).map(String::as_str).unwrap_or("");
                match arg.to_ascii_lowercase().as_str() {
                    "-name" | "/name" if target(value) => {
                        return illegal("Stop-Process would terminate Feather's own process");
                    }
                    "-id" | "/id" if value == own_pid => {
                        return illegal("Stop-Process would terminate Feather's own process");
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }
    Ok(())
}
fn check_depth(command: &str, depth: usize) -> Result<(), String> {
    if depth > 4 {
        return illegal("nested shell command exceeds policy inspection depth");
    }
    let segments = lex(command);
    for words in segments {
        for (n, word) in words.iter().enumerate() {
            if matches!(word.as_str(), ">" | ">>" | "&>" | "&>>" | ">|") {
                let target = words.get(n + 1).map(String::as_str).unwrap_or("");
                if target != "/dev/null" && target != "NUL" && !target.starts_with('&') {
                    return illegal("shell redirection would write a file");
                }
            }
        }
        let Some(index) = words.iter().position(|w| {
            !w.contains('=')
                && !matches!(
                    w.as_str(),
                    "command" | "exec" | "env" | "sudo" | "do" | "then"
                )
        }) else {
            continue;
        };
        let program = words[index]
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or("")
            .trim_end_matches(".exe")
            .to_ascii_lowercase();
        let args = &words[index + 1..];
        // Never let a tool call terminate the runtime that issued it.
        check_self_kill(&program, args)?;
        match program.as_str() {
            "eval" => return illegal("eval cannot be safely preflighted"),
            "bash" | "sh" | "zsh" => {
                if let Some(n) = args
                    .iter()
                    .position(|w| w.starts_with('-') && w.contains('c'))
                {
                    if let Some(script) = args.get(n + 1) {
                        check_depth(script, depth + 1)?;
                    }
                }
            }
            "sed" | "perl"
                if args.iter().any(|a| {
                    a.starts_with('-')
                        && (a == "--in-place"
                            || a.starts_with("--in-place=")
                            || (!a.starts_with("--") && a.contains('i')))
                }) =>
            {
                return illegal("in-place text edits belong to anchor tools")
            }
            "python" | "python3" | "py" | "node" | "ruby" => {
                if let Some(n) = args
                    .iter()
                    .position(|w| matches!(w.as_str(), "-c" | "-e" | "--eval"))
                {
                    if let Some(script) = args.get(n + 1) {
                        let script = script.to_ascii_lowercase();
                        if [
                            "write_text(",
                            "write_bytes(",
                            "writefile",
                            ".write(",
                            ".writelines(",
                            "unlink(",
                            "remove(",
                            "rename(",
                        ]
                        .iter()
                        .any(|s| script.contains(s))
                        {
                            return illegal("inline script would edit files outside anchor tools");
                        }
                    }
                }
            }
            "git" => {
                if let Some(n) = args
                    .iter()
                    .position(|a| matches!(a.as_str(), "reset" | "clean"))
                {
                    if args[n] == "reset" && args[n + 1..].iter().any(|a| a == "--hard") {
                        return illegal("git reset --hard can destroy user work");
                    }
                    if args[n] == "clean"
                        && args[n + 1..].iter().any(|a| {
                            a == "--force"
                                || a.starts_with('-') && !a.starts_with("--") && a.contains('f')
                        })
                    {
                        return illegal("git clean --force can destroy untracked user work");
                    }
                }
            }
            _ => {}
        }
    }
    Ok(())
}
// Quote-aware tokens preserve literal script arguments for nested preflight.
fn lex(text: &str) -> Vec<Vec<String>> {
    let mut segments = Vec::new();
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quote = None;
    let mut escaped = false;
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if escaped {
            word.push(ch);
            escaped = false;
            continue;
        }
        if ch == '\\' && quote != Some('\'') {
            escaped = true;
            continue;
        }
        if let Some(q) = quote {
            if ch == q {
                quote = None;
            } else {
                word.push(ch);
            }
            continue;
        }
        if ch == '\'' || ch == '"' {
            quote = Some(ch);
            continue;
        }
        if ch.is_whitespace() {
            if !word.is_empty() {
                words.push(std::mem::take(&mut word));
            }
            if ch == '\n' && !words.is_empty() {
                segments.push(std::mem::take(&mut words));
            }
            continue;
        }
        if matches!(ch, ';' | '|' | '&') {
            if ch == '&' && chars.peek() == Some(&'>') {
                if !word.is_empty() {
                    words.push(std::mem::take(&mut word));
                }
                let mut op = String::from("&>");
                chars.next();
                if chars.peek() == Some(&'>') {
                    chars.next();
                    op.push('>');
                }
                words.push(op);
                continue;
            }
            if !word.is_empty() {
                words.push(std::mem::take(&mut word));
            }
            if !words.is_empty() {
                segments.push(std::mem::take(&mut words));
            }
            continue;
        }
        if ch == '>' {
            if !word.is_empty() {
                words.push(std::mem::take(&mut word));
            }
            let mut op = String::from(">");
            if matches!(chars.peek(), Some('>') | Some('|')) {
                op.push(chars.next().unwrap());
            }
            if chars.peek() == Some(&'&') {
                chars.next();
                op.push('&');
            }
            words.push(op);
            continue;
        }
        word.push(ch);
    }
    if !word.is_empty() {
        words.push(word);
    }
    if !words.is_empty() {
        segments.push(words);
    }
    segments
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn clear_violations_are_blocked_before_a_whole_command_chain_runs() {
        for command in [
            "printf before; sed -i 's/a/b/' source.rs",
            "echo unsafe > file.rs",
            "bash -c 'git reset --hard'",
            "env git clean -fd",
            "python -c \"open('file','w').write('oops')\"",
        ] {
            assert!(
                check(command).unwrap_err().contains("ILLEGAL_COMMAND"),
                "{command}"
            );
        }
        for command in [
            "git add -- source.rs",
            "git status; git diff",
            "printf 'text' 2>/dev/null",
            "sed -n '1,10p' source.rs",
            "cargo test 2>&1 | tail -12",
            "python -c \"print('hello')\"",
            "for n in A B; do printf '%s' \"$n\"; done",
        ] {
            assert!(check(command).is_ok(), "{command}");
        }
    }

    #[test]
    fn commands_that_would_kill_feather_itself_are_refused() {
        let own = std::process::id().to_string();
        for command in [
            "taskkill /IM feather-gui.exe /F".to_string(),
            "taskkill /F /IM feather-gui.exe".to_string(),
            "taskkill /IM feather.exe /T /F".to_string(),
            format!("taskkill /PID {own} /F"),
            "Stop-Process -Name feather-gui -Force".to_string(),
            format!("kill -9 {own}"),
            "pkill feather-gui".to_string(),
            "killall feather".to_string(),
            // Nested shells must be inspected too, or `bash -c` becomes a bypass.
            "bash -c 'taskkill /IM feather-gui.exe /F'".to_string(),
        ] {
            let err = check(&command).unwrap_err();
            assert!(err.contains("ILLEGAL_COMMAND"), "{command} -> {err}");
        }
    }

    #[test]
    fn terminating_other_processes_stays_allowed() {
        for command in [
            // A pid that is not ours, and a name that is not Feather.
            "taskkill /PID 999999 /F",
            "taskkill /IM notepad.exe /F",
            "Stop-Process -Name notepad",
            "pkill notepad",
            "kill 999999",
        ] {
            assert!(check(command).is_ok(), "{command}");
        }
    }
}
