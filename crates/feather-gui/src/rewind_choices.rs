//! Bounded background rewind labels from a shared snapshot read handle, never
//! from a try_lock of the agent (which may be held for an entire response).
use feather_core::snapshot::SnapshotStore;
use std::sync::mpsc;

pub fn query(input:&str)->Option<&str> {
    if input=="/rewind" {return Some("");}
    input.strip_prefix("/rewind ").map(str::trim)
}
#[derive(Default)]
pub struct Turns {
    pub rows:Vec<(u64,String)>,
    pub loaded:bool,
    pub revision:u64,
    pub error:Option<String>,
    store:Option<SnapshotStore>,
    active:bool,
    pending:Option<mpsc::Receiver<Result<Vec<(u64,String)>,String>>>,
}
impl Turns {
    pub fn attach(&mut self,store:Option<SnapshotStore>) {
        let revision=self.revision.wrapping_add(1);
        *self=Self{store,revision,..Default::default()};
    }
    pub fn invalidate(&mut self) {
        self.revision=self.revision.wrapping_add(1);self.rows.clear();self.loaded=false;self.error=None;self.pending=None;
    }
    pub fn sync(&mut self,input:&str,ctx:&eframe::egui::Context){
        if query(input).is_none(){self.active=false;return;}
        if !self.active&&self.error.is_some(){self.invalidate();}
        self.active=true;self.request(ctx);
    }
    pub fn request(&mut self,ctx:&eframe::egui::Context) {
        if self.loaded||self.pending.is_some() {return;}
        let Some(store)=self.store.clone() else {self.loaded=true;return;};
        let(tx,rx)=mpsc::channel();self.pending=Some(rx);
        let ctx=ctx.clone();
        std::thread::spawn(move|| {
            let result=store.turn_labels(128).map_err(|e|e.to_string());
            let _=tx.send(result);ctx.request_repaint();
        });
    }
    pub fn poll(&mut self) {
        let Some(rx)=&self.pending else{return;};
        match rx.try_recv() {
            Ok(result)=>{
                self.revision=self.revision.wrapping_add(1);
                match result {Ok(rows)=>self.rows=rows,Err(error)=>self.error=Some(error)}
                self.pending=None;self.loaded=true;
            },
            Err(mpsc::TryRecvError::Disconnected)=>{
                self.pending=None;self.loaded=true;self.error=Some("Turn list worker disconnected; reopen the menu to retry.".into());
            },
            Err(mpsc::TryRecvError::Empty)=>{},
        }
    }
    pub fn suggestions(&self,input:&str)->Vec<(String,String)> {
        let Some(query)=query(input) else{return Vec::new();};let query=query.to_lowercase();
        self.rows.iter().filter(|(turn,label)|turn.to_string().starts_with(&query)||label.to_lowercase().contains(&query))
            .map(|(turn,label)|(format!("/rewind {turn}"),label.clone())).collect()
    }
}

#[cfg(test)]mod tests {
    use super::*;
    use feather_core::message::Message;
    #[test]fn background_menu_reads_latest_committed_turns_and_refreshes() {
        let dir=std::env::temp_dir().join(feather_core::provider::gen_id("turn-menu"));
        let store=SnapshotStore::open(&dir).unwrap();
        store.snapshot(1,&[Message::user_text("fix the parser")]).unwrap();
        let mut choices=Turns::default();choices.attach(Some(store.clone()));let ctx=eframe::egui::Context::default();
        let wait=|choices:&mut Turns|{let deadline=std::time::Instant::now()+std::time::Duration::from_secs(10);while !choices.loaded {choices.poll();assert!(std::time::Instant::now()<deadline);std::thread::sleep(std::time::Duration::from_millis(5));}};
        choices.request(&ctx);wait(&mut choices);
        assert_eq!(choices.suggestions("/rewind"),vec![("/rewind 1".into(),"fix the parser".into())]);
        store.snapshot(2,&[Message::user_text("fix the parser"),Message::user_text("add tests")]).unwrap();
        choices.invalidate();choices.request(&ctx);wait(&mut choices);
        assert_eq!(choices.rows.len(),2);assert_eq!(choices.suggestions("/rewind tests")[0].0,"/rewind 2");
        drop(choices);drop(store);std::fs::remove_dir_all(dir).unwrap();
    }
}
