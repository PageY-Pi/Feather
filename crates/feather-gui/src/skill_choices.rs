//! Metadata-only skill completion. Discovery runs off the UI thread each time
//! the skill menu is opened; selecting a skill never reads its instruction body.
use eframe::egui;
use feather::resources::ResourceLoad;
use std::{path::Path,sync::mpsc};

pub fn query(input:&str)->Option<&str> {
    if input=="/skill" {return Some("");}
    if let Some(query)=input.strip_prefix("/skill:") {
        // A trailing space means a skill was chosen: let the user write/send a
        // request instead of having Enter repeatedly select the same skill.
        return (!query.chars().any(char::is_whitespace)).then_some(query);
    }
    let query=input.strip_prefix("/skill ")?.trim_start();
    (!query.chars().any(char::is_whitespace)).then_some(query)
}

#[derive(Default)]
pub struct Skills {
    pub resources:ResourceLoad,
    pub loaded:bool,
    pub revision:u64,
    active:bool,
    pending:Option<mpsc::Receiver<ResourceLoad>>,
}
impl Skills {
    pub fn sync(&mut self,input:&str,root:&Path,home:&Path,ctx:&egui::Context) {
        if query(input).is_none() {self.active=false;return;}
        if self.active {return;}
        self.revision=self.revision.wrapping_add(1);
        self.active=true;self.loaded=false;self.resources=ResourceLoad::default();
        let(tx,rx)=mpsc::channel();self.pending=Some(rx);
        let(root,home,ctx)=(root.to_path_buf(),home.to_path_buf(),ctx.clone());
        std::thread::spawn(move|| {
            let resources=feather::config::resources_for(&root,&home);
            let _=tx.send(resources);ctx.request_repaint();
        });
    }
    pub fn poll(&mut self) {
        let Some(rx)=&self.pending else{return;};
        match rx.try_recv() {
            Ok(resources)=>{self.resources=resources;self.revision=self.revision.wrapping_add(1);self.loaded=true;self.pending=None;},
            Err(mpsc::TryRecvError::Disconnected)=>{self.loaded=true;self.pending=None;},
            Err(mpsc::TryRecvError::Empty)=>{},
        }
    }
    pub fn suggestions(&self,input:&str)->Vec<(String,String,bool)> {
        let Some(query)=query(input) else{return Vec::new();};let query=query.to_lowercase();
        self.resources.skills.iter()
            .filter(|s|s.name.to_lowercase().contains(&query)||s.description.to_lowercase().contains(&query))
            .map(|s|(format!("/skill:{} ",s.name),s.description.clone(),false)).collect()
    }
}

#[cfg(test)]mod tests {
    use super::*;
    #[test]fn selected_skill_is_not_reselected_when_sending_a_request() {
        assert_eq!(query("/skill"),Some(""));assert_eq!(query("/skill "),Some(""));
        assert_eq!(query("/skill doc"),Some("doc"));assert_eq!(query("/skill:doc"),Some("doc"));
        assert_eq!(query("/skill:docx "),None);assert_eq!(query("/skill:docx write a report"),None);
        assert_eq!(query("/skill docx write a report"),None);assert_eq!(query("/skills"),None);
    }
    #[test]fn discovery_on_reopen_finds_new_skills_without_loading_bodies() {
        let home=std::env::temp_dir().join(feather_core::provider::gen_id("skill-menu"));let root=home.join("workspace");
        std::fs::create_dir_all(&root).unwrap();let mut choices=Skills::default();let ctx=egui::Context::default();
        let wait=|choices:&mut Skills|{let deadline=std::time::Instant::now()+std::time::Duration::from_secs(10);while !choices.loaded {choices.poll();assert!(std::time::Instant::now()<deadline);std::thread::sleep(std::time::Duration::from_millis(5));}};
        choices.sync("/skill ",&root,&home,&ctx);wait(&mut choices);assert!(choices.suggestions("/skill ").is_empty());
        let dir=home.join("skills/demo");std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("SKILL.md"),"---\nname: demo\ndescription: Test documents\n---\nPRIVATE_BODY").unwrap();
        choices.sync("",&root,&home,&ctx);choices.sync("/skill",&root,&home,&ctx);wait(&mut choices);
        assert_eq!(choices.suggestions("/skill document"),vec![("/skill:demo ".into(),"Test documents".into(),false)]);
        assert!(!format!("{:?}",choices.resources).contains("PRIVATE_BODY"));drop(choices);std::fs::remove_dir_all(home).unwrap();
    }
}
