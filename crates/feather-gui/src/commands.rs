//! GUI slash commands and their single source of truth for suggestions/help.
#[derive(Clone,Copy)]
pub struct Command {pub name:&'static str,pub description:&'static str,pub args:bool}
pub const COMMANDS:&[Command]=&[
    Command{name:"new",description:"Start a fresh session",args:false},
    Command{name:"resume",description:"Choose a saved session (Del deletes)",args:false},
    Command{name:"skill",description:"Choose a skill, or /skill <name> <request>",args:true},
    Command{name:"proxy",description:"Choose or turn off Feather's request proxy",args:false},
    Command{name:"model",description:"Choose a model, or /model provider/id",args:true},
    Command{name:"think",description:"Set effort: off, low, med, high, xhigh, max",args:true},
    Command{name:"thinking",description:"Alias for /think",args:true},
    Command{name:"add",description:"Add APIs and manage saved models",args:false},
    Command{name:"settings",description:"Open settings and keyboard shortcuts",args:false},
    Command{name:"help",description:"Show available commands",args:false},
    Command{name:"hotkeys",description:"Show keyboard shortcuts",args:false},
    Command{name:"tools",description:"Fold/unfold tool output cards",args:false},
    Command{name:"thoughts",description:"Fold/unfold thinking cards",args:false},
    Command{name:"stop",description:"Stop the running response",args:false},
    Command{name:"snapshot",description:"Show the current committed turn",args:false},
    Command{name:"rewind",description:"Restore context and files: /rewind <turn>",args:true},
    Command{name:"quit",description:"Close Feather",args:false},
];
pub fn suggestions(input:&str)->Vec<&'static Command>{
    let Some(query)=input.strip_prefix('/') else{return Vec::new()};
    if query.chars().any(char::is_whitespace){return Vec::new()}
    let query=query.to_ascii_lowercase();
    COMMANDS.iter().filter(|c|c.name.starts_with(&query)).collect()
}
pub fn parse(input:&str)->Option<(&str,&str)>{
    let rest=input.strip_prefix('/')?;
    let (name,args)=rest.split_once(char::is_whitespace).unwrap_or((rest,""));
    Some((name,args.trim()))
}
#[cfg(test)]mod tests{
    use super::*;
    #[test]fn only_leading_slash_suggests(){
        assert_eq!(suggestions("/").len(),COMMANDS.len());
        assert!(suggestions("hello /").is_empty());assert!(suggestions(" /model").is_empty());
        assert_eq!(suggestions("/rew")[0].name,"rewind");assert!(suggestions("/think high").is_empty());
    }
}
