//! Cooperative close / relaunch coordination shared by eframe and CPU host.
//! A renderer restart is scheduled only after async preference/handoff commit;
//! it is launched AFTER native teardown, not inside egui update or Drop.
use crate::renderer_choice::Backend;
use serde::{Deserialize, Serialize};
use std::{path::{Path,PathBuf},sync::{mpsc,Mutex},time::{Duration,Instant}};
pub const CLOSE_GRACE:Duration=Duration::from_secs(3);
const PREPARE_TIMEOUT:Duration=Duration::from_secs(10);
const RESTART_WAIT_TIMEOUT:Duration=Duration::from_secs(15);

#[derive(Clone,Debug,Serialize,Deserialize,PartialEq)]
pub struct Handoff {
    pub token:String,
    pub session:PathBuf,
    pub draft:String,
    pub show_thinking:bool,
    pub expand_tools:bool,
}
pub struct Restart {
    exe:PathBuf,
    root:PathBuf,
    home:PathBuf,
    backend:Backend,
    token:String,
}
static RESTART:Mutex<Option<Restart>>=Mutex::new(None);
pub struct Exit {
    pub backend:Option<Backend>,
    since:Instant,
    preparation:Option<mpsc::Receiver<Result<Restart,String>>>,
}
impl Exit {
    pub fn close()->Self { Self{backend:None,since:Instant::now(),preparation:None} }
    pub fn restart(home:PathBuf,root:PathBuf,backend:Backend,handoff:Handoff)->Self {
        let(tx,rx)=mpsc::channel();
        std::thread::spawn(move||{let result=prepare(home,root,backend,handoff);let _=tx.send(result.map_err(|e|e.to_string()));});
        Self{backend:Some(backend),since:Instant::now(),preparation:Some(rx)}
    }
    pub fn poll(&mut self,busy:bool)->Result<bool,String>{
        if let Some(rx)=&self.preparation {
            match rx.try_recv(){
                Ok(Ok(restart))=>{*RESTART.lock().unwrap_or_else(|e|e.into_inner())=Some(restart);self.preparation=None;},
                Ok(Err(error))=>return Err(error),
                Err(mpsc::TryRecvError::Disconnected)=>return Err("Restart preparation stopped".into()),
                Err(mpsc::TryRecvError::Empty)=>{
                    if self.since.elapsed()>PREPARE_TIMEOUT{return Err("Renderer settings are busy. Please try again.".into());}
                    return Ok(false);
                }
            }
        }
        if self.backend.is_some() && busy {
            if self.since.elapsed()>=CLOSE_GRACE {return Err("Agent is still finishing. Retry the renderer switch when idle.".into());}
            return Ok(false);
        }
        Ok(!busy||self.since.elapsed()>=CLOSE_GRACE)
    }
}
fn restart_exe(exe:PathBuf)->PathBuf {
    // install-local can replace a running EXE by renaming its old image. Relaunch
    // the current installed binary, not a *.previous-* backup of an old build.
    if exe.file_name().and_then(|v|v.to_str()).is_some_and(|v|v.starts_with("feather-gui.previous-")){
        let latest=exe.with_file_name("feather-gui.exe");if latest.is_file(){return latest;}
    }
    exe
}
fn prepare(home:PathBuf,root:PathBuf,backend:Backend,handoff:Handoff)->anyhow::Result<Restart>{
    let exe=restart_exe(std::env::current_exe()?);
    anyhow::ensure!(exe.is_file(),"Feather executable is not available for restart");
    commit_handoff(&home,backend,&handoff)?;
    Ok(Restart{exe,root,home,backend,token:handoff.token})
}
fn commit_handoff(home:&Path,backend:Backend,handoff:&Handoff)->std::io::Result<()> {
    feather::config_store::update(home,"gui",|v|{v["renderer"]=serde_json::json!(backend);v["restart"]=serde_json::json!(handoff);})
}
pub fn take_handoff(home:&Path,token:&str)->std::io::Result<Option<Handoff>>{
    let mut found=None;
    feather::config_store::update(home,"gui",|v|{
        if v["restart"]["token"].as_str()==Some(token){
            found=serde_json::from_value(v["restart"].take()).ok();
        }
    })?;
    Ok(found)
}
pub fn startup_handoff()->std::io::Result<Option<Handoff>>{
    let Some(token)=std::env::var("FEATHER_GUI_RESTART_TOKEN").ok() else{return Ok(None);};
    let home=std::env::var_os("FEATHER_GUI_RESTART_HOME").map(PathBuf::from).unwrap_or_else(feather::config::feather_home);
    let result=take_handoff(&home,&token);
    std::env::remove_var("FEATHER_GUI_RESTART_TOKEN");std::env::remove_var("FEATHER_GUI_RESTART_HOME");
    result
}
pub fn cancel_pending() { let _=RESTART.lock().unwrap_or_else(|e|e.into_inner()).take(); }
pub fn launch_pending()->std::io::Result<()> {
    let Some(restart)=RESTART.lock().unwrap_or_else(|e|e.into_inner()).take() else{return Ok(());};
    let mut command=std::process::Command::new(restart.exe);
    command.arg(restart.root).env("FEATHER_GUI_RENDERER",restart.backend.env())
        .env("FEATHER_GUI_RESTART_TOKEN",restart.token).env("FEATHER_GUI_RESTART_HOME",restart.home)
        .env("FEATHER_GUI_WAIT_FOR_PARENT",std::process::id().to_string());
    #[cfg(windows)]{use std::os::windows::process::CommandExt;command.creation_flags(0x08000000);}
    command.spawn()?;
    Ok(())
}
pub fn wait_for_parent()->std::io::Result<()> {
    let pid=std::env::var("FEATHER_GUI_WAIT_FOR_PARENT").ok().and_then(|v|v.parse::<u32>().ok());
    std::env::remove_var("FEATHER_GUI_WAIT_FOR_PARENT");
    #[cfg(windows)] if let Some(pid)=pid {
        use windows_sys::Win32::{Foundation::{CloseHandle,WAIT_TIMEOUT},System::Threading::{OpenProcess,WaitForSingleObject}};
        if pid==std::process::id(){return Err(std::io::Error::other("cannot wait for this process"));}
        // SAFETY: query/synchronize-only handle to our parent; no process is
        // killed or suspended. Release it before returning on every result.
        unsafe {
            let process=OpenProcess(0x00100000,0,pid);
            if !process.is_null(){
                let result=WaitForSingleObject(process,RESTART_WAIT_TIMEOUT.as_millis().min(u32::MAX as u128) as u32);CloseHandle(process);
                if result==WAIT_TIMEOUT{return Err(std::io::Error::other("previous Feather process did not finish closing"));}
            }
        }
    }
    #[cfg(not(windows))] let _=pid;
    Ok(())
}

#[cfg(test)]
mod tests{
    use super::*;
    #[test]
    fn handoff_preserves_exact_session_and_long_unicode_draft_once(){
        let home=std::env::temp_dir().join(feather_core::provider::gen_id("restart-test"));
        let h=Handoff{token:"test-token".into(),session:home.join("sessions/s123"),draft:"中文未发送🙂".repeat(10_000),show_thinking:false,expand_tools:true};
        commit_handoff(&home,Backend::OpenGl,&h).unwrap();
        assert!(take_handoff(&home,"wrong-token").unwrap().is_none());
        assert_eq!(take_handoff(&home,"test-token").unwrap(),Some(h));
        assert!(take_handoff(&home,"test-token").unwrap().is_none());
        assert_eq!(crate::renderer_choice::load(&home),Backend::OpenGl);
        std::fs::remove_dir_all(home).unwrap();
    }
    #[test]
    fn cooperative_close_is_bounded(){
        let mut exit=Exit::close();assert!(!exit.poll(true).unwrap());assert!(exit.poll(false).unwrap());
        exit.since=Instant::now()-CLOSE_GRACE;assert!(exit.poll(true).unwrap());
    }
}
