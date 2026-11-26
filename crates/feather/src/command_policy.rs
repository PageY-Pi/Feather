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
    illegal_with(
        reason,
        "use the `write`, `replace`, or `insert` tool instead",
    )
}

/// Refuse a command, naming the tool that does the equivalent safely.
///
/// The old text said "use the anchor file tools", which is internal jargon: the
/// tools are named `write`/`replace`/`insert`/`read`, and a model told to use
/// "anchor tools" has to guess. Every refusal now names a concrete next step,
/// because the point of the preflight is to redirect the call, not just stop it.
fn illegal_with(reason: &str, instead: &str) -> Result<(), String> {
    Err(format!(
        "[ILLEGAL_COMMAND] Blocked before execution: {reason}. \
         This is a recoverable tool error: {instead} and continue. \
         Allowed in bash: reading, searching, building, testing, and redirecting to \
         /dev/null or NUL."
    ))
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
    let segments = lex_quoted(command);
    for words in segments {
        for (n, (word, from_quotes)) in words.iter().enumerate() {
            // A redirect operator only counts when it was not quoted. Otherwise
            // `grep '>' f`, `awk 'BEGIN { print "a>b" }'` and any patch body
            // containing `->` were refused for writing a file they never touch.
            if !*from_quotes && matches!(word.as_str(), ">" | ">>" | "&>" | "&>>" | ">|") {
                let target = words.get(n + 1).map(|(w, _)| w.as_str()).unwrap_or("");
                if target != "/dev/null" && target != "NUL" && !target.starts_with('&') {
                    return illegal_with(
                        "shell redirection would write a file",
                        "write the file with the `write` tool (path + content), or edit it with `replace`",
                    );
                }
            }
        }
        // Everything below reasons about token text only; the quote flag is
        // needed just for the redirect scan above.
        let words: Vec<String> = words.into_iter().map(|(w, _)| w).collect();
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
            "eval" => {
                return illegal_with(
                    "eval cannot be safely preflighted",
                    "run the command directly instead",
                )
            }
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
                return illegal_with(
                    "in-place text edits are not allowed in bash",
                    "use `replace` with the anchors from `read`, or `anchor_grep` to locate the text first",
                )
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
                            return illegal_with(
                                "an inline script would write files from bash",
                                "make the edit with `write` or `replace`",
                            );
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
                        return illegal_with(
                            "git reset --hard can destroy user work",
                            "`git stash`, or undo a single edit with `undo_last_change`",
                        );
                    }
                    if args[n] == "clean"
                        && args[n + 1..].iter().any(|a| {
                            a == "--force"
                                || a.starts_with('-') && !a.starts_with("--") && a.contains('f')
                        })
                    {
                        return illegal_with(
                            "git clean --force can destroy untracked user work",
                            "delete the specific paths you created instead of everything untracked",
                        );
                    }
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// Lex into `(token, came_from_quotes)` pairs.
///
/// The quote flag exists because stripping quotes before the operator scan made a
/// quoted `>` indistinguishable from a redirect: `grep '>' notes.txt` writes
/// nothing, yet was refused as `shell redirection would write a file`. Operators
/// are only ever produced by the unquoted branches below, so the flag is exactly
/// what the redirect check needs.
fn lex_quoted(text: &str) -> Vec<Vec<(String, bool)>> {
    let mut segments = Vec::new();
    let mut words: Vec<(String, bool)> = Vec::new();
    let mut word = String::new();
    let mut word_quoted = false;
    let mut quote = None;
    let mut escaped = false;
    let mut chars = text.chars().peekable();

    // Flush the pending word, remembering whether any of it came from quotes.
    macro_rules! flush_word {
        () => {
            if !word.is_empty() {
                words.push((std::mem::take(&mut word), word_quoted));
                // Required, not redundant: this macro also runs between a
                // word and a following operator (`grep 'x' > f` flushes `x`
                // then pushes `>`), and the next word must start unquoted.
                // The compiler cannot see that path because `word_quoted` is
                // only read again after something pushes into `word`.
                #[allow(unused_assignments)]
                {
                    word_quoted = false;
                }
            }
        };
    }
    // Close the current segment (a `;`/`|`/newline boundary).
    macro_rules! end_segment {
        () => {
            if !words.is_empty() {
                segments.push(std::mem::take(&mut words));
            }
        };
    }
    // Push an operator token, which by construction is never quoted.
    macro_rules! push_op {
        ($op:expr) => {
            words.push(($op, false))
        };
    }

    while let Some(ch) = chars.next() {
        if escaped {
            word.push(ch);
            word_quoted = true;
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
                word_quoted = true;
            }
            continue;
        }
        if ch == '\'' || ch == '"' {
            quote = Some(ch);
            // An empty pair (`''`) still marks the token as quoted so that a
            // following operator scan does not treat a bare quote-derived token
            // as an operator.
            word_quoted = true;
            continue;
        }
        if ch.is_whitespace() {
            flush_word!();
            if ch == '\n' {
                end_segment!();
            }
            continue;
        }
        if matches!(ch, ';' | '|' | '&') {
            if ch == '&' && chars.peek() == Some(&'>') {
                flush_word!();
                let mut op = String::from("&>");
                chars.next();
                if chars.peek() == Some(&'>') {
                    chars.next();
                    op.push('>');
                }
                push_op!(op);
                continue;
            }
            flush_word!();
            end_segment!();
            continue;
        }
        if ch == '>' {
            flush_word!();
            let mut op = String::from(">");
            if matches!(chars.peek(), Some('>') | Some('|')) {
                op.push(chars.next().unwrap());
            }
            if chars.peek() == Some(&'&') {
                chars.next();
                op.push('&');
            }
            push_op!(op);
            continue;
        }
        word.push(ch);
    }
    flush_word!();
    end_segment!();
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

    /// A quoted `>` is an argument, not a redirect.
    ///
    /// The lexer strips quotes before the operator scan, so `grep '>' f` produced
    /// exactly the same token as `echo x > f` and was refused for writing a file
    /// it never touches. These are ordinary read-only commands that a shell user
    /// runs constantly, and the refusal taught the model to route around a tool
    /// that was simply wrong about them.
    #[test]
    fn a_quoted_angle_bracket_is_not_a_redirect() {
        for command in [
            "grep '>' notes.txt",
            "grep \">\" notes.txt",
            "rg --fixed-strings '=>' src/",
            "awk 'BEGIN { print \"a>b\" }'",
            "git log --format='%h -> %s'",
            // A patch body carried on stdin contains `->` inside a heredoc.
            "python -c 'print(\"=>\")'",
        ] {
            assert!(check(command).is_ok(), "{command} should be allowed");
        }
    }

    /// The unquoted forms must still be refused, or the fix above would have
    /// opened the hole it was meant to close.
    #[test]
    fn a_real_redirect_is_still_refused() {
        for command in [
            "echo hi > out.txt",
            "echo hi >> out.txt",
            "echo hi &> out.txt",
            "printf x 1> out.txt",
            // Mixed quoting: the operator itself is unquoted, so it still writes.
            "echo \"hi\" > out.txt",
            "echo hi >\"out.txt\"",
            "cat a b >combined.txt",
        ] {
            let err = check(command).unwrap_err();
            assert!(err.contains("ILLEGAL_COMMAND"), "{command} -> {err}");
        }
    }

    /// The documented carve-outs must keep working, since they are the only way
    /// to discard output.
    #[test]
    fn discarding_output_stays_allowed() {
        for command in [
            "cargo test > /dev/null",
            "cargo test 2>&1",
            "cargo test > NUL",
            "make 2>&1 | tail -20",
        ] {
            assert!(check(command).is_ok(), "{command}");
        }
    }
}
