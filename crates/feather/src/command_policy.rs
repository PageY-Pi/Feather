//! Conservative preflight for clearly disallowed shell edits/destructive git.
//! This is not a Bash sandbox: unknown programs can have side effects. The gate
//! blocks known violations before spawning, and returns a normal tool error so
//! the model can correct it instead of aborting the agent.
pub fn check(command:&str)->Result<(),String> {
    check_depth(command,0)
}
fn illegal(reason:&str)->Result<(),String> {
    Err(format!("[ILLEGAL_COMMAND] Blocked before execution: {reason}. Use the anchor file tools or an allowed inspection command. This is a recoverable tool error; continue with a safe alternative."))
}
fn check_depth(command:&str,depth:usize)->Result<(),String> {
    if depth>4{return illegal("nested shell command exceeds policy inspection depth");}
    let segments=lex(command);
    for words in segments {
        for (n,word) in words.iter().enumerate() {
            if matches!(word.as_str(),">"|">>"|"&>"|"&>>"|">|") {
                let target=words.get(n+1).map(String::as_str).unwrap_or("");
                if target!="/dev/null" && target!="NUL" && !target.starts_with('&') {return illegal("shell redirection would write a file");}
            }
        }
        let Some(index)=words.iter().position(|w|!w.contains('=')&&!matches!(w.as_str(),"command"|"exec"|"env"|"sudo"|"do"|"then")) else{continue;};
        let program=words[index].rsplit(['/', '\\']).next().unwrap_or("").trim_end_matches(".exe").to_ascii_lowercase();
        let args=&words[index+1..];
        match program.as_str() {
            "eval"=>return illegal("eval cannot be safely preflighted"),
            "bash"|"sh"|"zsh"=>{
                if let Some(n)=args.iter().position(|w|w.starts_with('-')&&w.contains('c')) {
                    if let Some(script)=args.get(n+1){check_depth(script,depth+1)?;}
                }
            },
            "sed"|"perl" if args.iter().any(|a|a.starts_with('-')&&(a=="--in-place"||a.starts_with("--in-place=")||(!a.starts_with("--")&&a.contains('i'))))=>return illegal("in-place text edits belong to anchor tools"),
            "python"|"python3"|"py"|"node"|"ruby"=>{
                if let Some(n)=args.iter().position(|w|matches!(w.as_str(),"-c"|"-e"|"--eval")) {
                    if let Some(script)=args.get(n+1) {
                        let script=script.to_ascii_lowercase();
                        if ["write_text(","write_bytes(","writefile", ".write(",".writelines(","unlink(","remove(","rename("].iter().any(|s|script.contains(s)) {return illegal("inline script would edit files outside anchor tools");}
                    }
                }
            },
            "git"=>{
                if let Some(n)=args.iter().position(|a|matches!(a.as_str(),"reset"|"clean")) {
                    if args[n]=="reset"&&args[n+1..].iter().any(|a|a=="--hard"){return illegal("git reset --hard can destroy user work");}
                    if args[n]=="clean"&&args[n+1..].iter().any(|a|a=="--force"||a.starts_with('-')&&!a.starts_with("--")&&a.contains('f')) {return illegal("git clean --force can destroy untracked user work");}
                }
            },
            _=>{},
        }
    }
    Ok(())
}
// Quote-aware tokens preserve literal script arguments for nested preflight.
fn lex(text:&str)->Vec<Vec<String>> {
    let mut segments=Vec::new();let mut words=Vec::new();let mut word=String::new();let mut quote=None;let mut escaped=false;
    let mut chars=text.chars().peekable();
    while let Some(ch)=chars.next() {
        if escaped{word.push(ch);escaped=false;continue;}
        if ch=='\\'&&quote!=Some('\''){escaped=true;continue;}
        if let Some(q)=quote {if ch==q{quote=None;}else{word.push(ch);}continue;}
        if ch=='\''||ch=='"'{quote=Some(ch);continue;}
        if ch.is_whitespace(){if !word.is_empty(){words.push(std::mem::take(&mut word));}if ch=='\n'&&!words.is_empty(){segments.push(std::mem::take(&mut words));}continue;}
        if matches!(ch,';'|'|'|'&') {
            if ch=='&'&&chars.peek()==Some(&'>'){if !word.is_empty(){words.push(std::mem::take(&mut word));}let mut op=String::from("&>");chars.next();if chars.peek()==Some(&'>'){chars.next();op.push('>');}words.push(op);continue;}
            if !word.is_empty(){words.push(std::mem::take(&mut word));}if !words.is_empty(){segments.push(std::mem::take(&mut words));}continue;
        }
        if ch=='>' {if !word.is_empty(){words.push(std::mem::take(&mut word));}let mut op=String::from(">");if matches!(chars.peek(),Some('>')|Some('|')){op.push(chars.next().unwrap());}if chars.peek()==Some(&'&'){chars.next();op.push('&');}words.push(op);continue;}
        word.push(ch);
    }
    if !word.is_empty(){words.push(word);}if !words.is_empty(){segments.push(words);}segments
}
#[cfg(test)]mod tests {
    use super::*;
    #[test]fn clear_violations_are_blocked_before_a_whole_command_chain_runs(){
        for command in ["printf before; sed -i 's/a/b/' source.rs","echo unsafe > file.rs","bash -c 'git reset --hard'","env git clean -fd","python -c \"open('file','w').write('oops')\""]{assert!(check(command).unwrap_err().contains("ILLEGAL_COMMAND"),"{command}");}
        for command in ["git add -- source.rs","git status; git diff","printf 'text' 2>/dev/null","sed -n '1,10p' source.rs","cargo test 2>&1 | tail -12","python -c \"print('hello')\"","for n in A B; do printf '%s' \"$n\"; done"]{assert!(check(command).is_ok(),"{command}");}
    }
}
