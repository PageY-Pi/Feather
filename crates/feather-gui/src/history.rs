//! The visual conversation is the original redb audit, never Session::messages
//! (which is the compacted LLM context). Read one bounded tail page on resume;
//! older pages use an independent read handle, not the Agent mutex or UI thread.
use crate::{rows_from_messages, ChatRow};
use feather_core::{session_store::{RawPage, SessionStore}, Session};
use std::{collections::{HashSet,VecDeque},sync::mpsc};

pub const PAGE_ROWS: usize = 96;
pub const PAGE_BYTES: usize = 1024 * 1024;
const RETAIN_ABOVE_VIEWPORT_PAGES: usize = 2;
#[derive(Default)]
struct LoadedPage {
    before: Option<u64>,
    keys: Vec<u64>,
}
#[derive(Default)]
pub struct History {
    store: Option<SessionStore>,
    before: Option<u64>,
    tail_before: Option<u64>,
    loaded: VecDeque<LoadedPage>,
    pending: Option<mpsc::Receiver<Result<RawPage,String>>>,
    tail_pending:Option<(u64,mpsc::Receiver<Result<RawPage,String>>)>,
    committed_before:Option<u64>,
    committed_key:Option<u64>,
    tail_refresh_ready:bool,
    pub error: Option<String>,
}
impl History {
    pub fn from_session(session:&Session)->Result<(Self,Vec<ChatRow>),String> {
        let Some(store)=session.transcript_store() else {
            // Synthetic/memory sessions have no audit database. Durable GUI
            // sessions ALWAYS take the raw path, including after compaction.
            return Ok((Self::default(),rows_from_messages(session.messages())));
        };
        let page=store.raw_page_before(None,PAGE_ROWS,PAGE_BYTES).map_err(|e|e.to_string())?;
        let messages:Vec<_>=page.entries.into_iter().map(|e|e.message).collect();
        Ok((Self{store:Some(store),before:page.next_before,tail_before:page.next_before,loaded:VecDeque::new(),pending:None,tail_pending:None,committed_before:None,committed_key:None,tail_refresh_ready:false,error:None},rows_from_messages(&messages)))
    }
    pub fn remaining(&self)->u64 {self.before.unwrap_or(0)}
    pub fn loading(&self)->bool {self.pending.is_some()||self.tail_pending.is_some()}
    pub fn paged(&self)->bool {self.tail_before.is_some()}
    /// Drop fully older redb pages that are above the viewport. The newest
    /// resume page (`PAGE_ROWS`) and any live rows after it are never owned by
    /// these groups, so they stay resident while old `String` payloads are
    /// released from `rows`. `first_visible_key` should be the key recorded by
    /// `transcript::show`; `at_bottom` permits dropping every older page.
    pub fn evict_above(&mut self,rows:&mut Vec<ChatRow>,first_visible_key:Option<u64>,at_bottom:bool)->usize {
        if self.loaded.is_empty()||self.pending.is_some(){return 0;}
        let evict=if at_bottom {
            self.loaded.len()
        } else if let Some(key)=first_visible_key {
            if !rows.iter().any(|row|row.ui_key==key){return 0;}
            let visible_group=self.loaded.iter().position(|page|page.keys.contains(&key)).unwrap_or(self.loaded.len());
            visible_group.saturating_sub(RETAIN_ABOVE_VIEWPORT_PAGES)
        } else {0};
        if evict==0{return 0;}
        let mut keys=HashSet::new();let mut restore=self.before;
        for _ in 0..evict {
            if let Some(page)=self.loaded.pop_front(){restore=page.before;keys.extend(page.keys);}
        }
        self.before=restore.or(self.tail_before);
        let before_len=rows.len();
        rows.retain(|row|!keys.contains(&row.ui_key));
        before_len-rows.len()
    }
    /// Durable run completed; defer replacement until the user is at the bottom.
    /// Audit remains the source of truth; rebuilding the tail avoids inventing a
    /// raw cursor from ephemeral UI keys and keeps offloaded rows reloadable.
    pub fn durable_tail_changed(&mut self){self.tail_refresh_ready=true;}
    pub fn committed(&mut self,raw_before:u64,rows:&[ChatRow]){self.committed_before=Some(raw_before);self.committed_key=rows.last().map(|r|r.ui_key);self.tail_refresh_ready=true;}
    pub fn bound_live_tail(&mut self,rows:&[ChatRow],at_bottom:bool,ctx:&eframe::egui::Context){
        if !at_bottom||!self.tail_refresh_ready||self.pending.is_some()||self.tail_pending.is_some(){return;}

        if rows.iter().any(|r|r.streaming)&&self.committed_before.is_none(){return;}
        if let Some(key)=self.committed_key{if !rows.iter().any(|r|r.ui_key==key){return;}}
        let bytes:usize=rows.iter().map(|r|r.text.len()+r.thinking.len()+r.tool_args.len()+r.tool_output.as_ref().map_or(0,String::len)).sum();
        if rows.len()<=256&&bytes<=2*PAGE_BYTES {return;}
        let Some(store)=self.store.clone() else{return;};
        let Some(last)=self.committed_key.and_then(|key|rows.iter().find(|r|r.ui_key==key)).or_else(||if self.committed_before.is_none(){rows.last()}else{None}) else{return;};let key=last.ui_key;
        let(tx,rx)=mpsc::channel();self.tail_pending=Some((key,rx));let ctx=ctx.clone();
        let before=self.committed_before;
        std::thread::spawn(move||{let page=store.raw_page_before(before,PAGE_ROWS,PAGE_BYTES).map_err(|e|e.to_string());let _=tx.send(page);ctx.request_repaint();});
    }
    fn poll_tail(&mut self,rows:&mut Vec<ChatRow>)->bool {
        let Some((key,rx))=&self.tail_pending else{return false;};let key=*key;
        let result=match rx.try_recv(){Ok(r)=>r,Err(mpsc::TryRecvError::Empty)=>return false,Err(_)=>Err("History tail reader stopped".into())};
        self.tail_pending=None;
        let Some(boundary)=rows.iter().position(|r|r.ui_key==key)else{return false;};
        if rows.iter().skip(boundary+1).any(|r|!r.streaming&&r.kind!=crate::RowKind::User){return false;}
        let mut live=rows.split_off(boundary+1);
        match result {
            Ok(page)=>{
                if self.committed_before.is_some()&&page.before!=self.committed_before{rows.append(&mut live);return false;}
                let mut tail=rows_from_messages(&page.entries.into_iter().map(|e|e.message).collect::<Vec<_>>());
                // Preserve matching tail UI fold state/identity, but payloads come
                // from the complete persisted audit rather than LLM compaction.
                preserve_tail_keys(&mut tail,rows);
                self.committed_key=tail.last().map(|r|r.ui_key);
                tail.append(&mut live);*rows=tail;self.before=page.next_before;self.tail_before=page.next_before;self.loaded.clear();self.tail_refresh_ready=false;true
            },Err(e)=>{rows.append(&mut live);self.error=Some(e);false}
        }
    }
    pub fn request_older(&mut self,ctx:&eframe::egui::Context) {
        if self.pending.is_some()||self.tail_pending.is_some(){return;}
        let (Some(before),Some(store))=(self.before,self.store.clone()) else{return;};
        let (tx,rx)=mpsc::channel();self.pending=Some(rx);self.error=None;
        let ctx=ctx.clone();
        std::thread::spawn(move|| {
            let page=store.raw_page_before(Some(before),PAGE_ROWS,PAGE_BYTES).map_err(|e|e.to_string());
            let _=tx.send(page);ctx.request_repaint();
        });
    }
    pub fn poll(&mut self,rows:&mut Vec<ChatRow>)->bool {
        if self.poll_tail(rows){return true;}
        let Some(rx)=&self.pending else{return false;};
        let result=match rx.try_recv() {
            Ok(result)=>result,
            Err(mpsc::TryRecvError::Empty)=>return false,
            Err(mpsc::TryRecvError::Disconnected)=>Err("History reader stopped; click to retry.".into()),
        };
        self.pending=None;
        match result {
            Ok(page)=>{
                if page.before!=self.before {return false;}
                let page_before=page.before;
                self.before=page.next_before;
                let messages:Vec<_>=page.entries.into_iter().map(|e|e.message).collect();
                let keys=prepend(rows_from_messages(&messages),rows);
                if !keys.is_empty(){self.loaded.push_front(LoadedPage{before:page_before,keys});}
                true
            },
            Err(error)=>{self.error=Some(error);false},
        }
    }
}
// Match backwards one-to-one in order, including the complete final payload.
// Repeated prompts/answers must not all borrow the same last row identity.
fn preserve_tail_keys(tail:&mut [ChatRow],old:&[ChatRow]){
    let mut end=old.len();
    for row in tail.iter_mut().rev(){
        if let Some(index)=old[..end].iter().rposition(|r|r.kind==row.kind&&r.text==row.text&&r.thinking==row.thinking&&r.tool_id==row.tool_id&&r.tool_output==row.tool_output){
            row.ui_key=old[index].ui_key;row.expanded=old[index].expanded;end=index;
        }
    }
}
fn prepend(mut older:Vec<ChatRow>,current:&mut Vec<ChatRow>) -> Vec<u64> {
    let current_keys:HashSet<_>=current.iter().map(|r|r.ui_key).collect();

    // A page can split tool_use/tool_result. Keep one card at the call's original
    // place, with its result/progress and stable row identity from the next page.
    current.retain(|row| {
        if let Some(id)=&row.tool_id {
            if let Some(call)=older.iter_mut().find(|old|old.tool_id.as_ref()==Some(id)) {
                call.ui_key=row.ui_key;
                if row.tool_output.is_some() {
                    call.tool_output=row.tool_output.clone();call.tool_error=row.tool_error;
                    call.streaming=row.streaming;call.elapsed=row.elapsed;
                }
                if call.tool_args.is_empty(){call.tool_args=row.tool_args.clone();}
                return false;
            }
        }
        true
    });
    let owned=older.iter().filter(|r|!current_keys.contains(&r.ui_key)).map(|row|row.ui_key).collect();
    older.append(current);*current=older;
    owned
}

#[cfg(test)]
mod tests {
    #[test]
    fn repeated_identical_prompts_retain_distinct_tail_keys(){
        let old:Vec<_>=(0..20).map(|_|ChatRow::new(crate::RowKind::User,"continue")).collect();
        let mut tail:Vec<_>=(0..10).map(|_|ChatRow::new(crate::RowKind::User,"continue")).collect();
        preserve_tail_keys(&mut tail,&old);let keys:HashSet<_>=tail.iter().map(|r|r.ui_key).collect();assert_eq!(keys.len(),tail.len());
        assert_eq!(tail.iter().map(|r|r.ui_key).collect::<Vec<_>>(),old[10..].iter().map(|r|r.ui_key).collect::<Vec<_>>());
    }
    #[test]
    fn offload_committed_prefix_preserves_current_stream_and_cursor(){
        let home=std::env::temp_dir().join(feather_core::provider::gen_id("live-stream-bound"));let root=home.join("workspace");std::fs::create_dir_all(&root).unwrap();let mut session=Session::new(&home,&root).unwrap();
        for n in 0..400{session.push(Message::user_text(format!("durable {n}"))).unwrap();}let(mut history,_)=History::from_session(&session).unwrap();let mut rows=rows_from_messages(&session.raw_messages().unwrap());
        history.committed(400,&rows);let mut stream=ChatRow::new(crate::RowKind::Assistant,"CURRENT LIVE TEXT");stream.streaming=true;let key=stream.ui_key;rows.push(stream);
        let ctx=eframe::egui::Context::default();history.bound_live_tail(&rows,true,&ctx);let start=std::time::Instant::now();while history.loading(){history.poll(&mut rows);assert!(start.elapsed()<std::time::Duration::from_secs(3));std::thread::sleep(std::time::Duration::from_millis(2));}
        assert_eq!(rows.len(),PAGE_ROWS+1);assert_eq!(rows.last().unwrap().ui_key,key);assert!(rows.last().unwrap().streaming);assert_eq!(rows[0].text,"durable 304");drop(history);drop(session);std::fs::remove_dir_all(home).unwrap();
    }
    #[test]
    fn continuing_live_dialog_rebases_to_bounded_redb_tail_and_reloads_originals(){
        let home=std::env::temp_dir().join(feather_core::provider::gen_id("live-tail-bound"));let root=home.join("workspace");std::fs::create_dir_all(&root).unwrap();
        let mut session=Session::new(&home,&root).unwrap();let(mut history,mut rows)=History::from_session(&session).unwrap();
        for n in 0..400{let message=Message::user_text(format!("live request {n}"));session.push(message.clone()).unwrap();rows.extend(rows_from_messages(&[message]));}
        history.durable_tail_changed();let ctx=eframe::egui::Context::default();history.bound_live_tail(&rows,true,&ctx);
        let start=std::time::Instant::now();while history.loading(){history.poll(&mut rows);assert!(start.elapsed()<std::time::Duration::from_secs(3));std::thread::sleep(std::time::Duration::from_millis(2));}
        assert_eq!(rows.len(),PAGE_ROWS);assert_eq!(rows[0].text,"live request 304");assert!(history.remaining()>0);
        while history.remaining()>0{history.request_older(&ctx);while history.loading(){history.poll(&mut rows);std::thread::sleep(std::time::Duration::from_millis(2));}}
        assert_eq!(rows.len(),400);assert_eq!(rows[0].text,"live request 0");drop(history);drop(session);std::fs::remove_dir_all(home).unwrap();
    }
    #[test]
    fn eviction_does_not_drop_a_tail_tool_merged_across_page_boundary() {
        let call=feather_core::message::ToolCall{id:"tail-tool".into(),name:"read".into(),arguments:serde_json::json!({"path":"file.rs"})};
        let old=Message::assistant(vec![ContentBlock::ToolCall(call)],"synthetic",Api::Faux,Usage::default(),StopReason::ToolUse);
        let mut rows=rows_from_messages(&[Message::tool_result("tail-tool","read","keep tail result",false)]);
        let tail_key=rows[0].ui_key;
        let owned=prepend(rows_from_messages(&[Message::user_text("old prompt"),old]),&mut rows);
        let mut history=History::default();history.before=None;history.tail_before=Some(12);
        history.loaded.push_back(LoadedPage{before:Some(12),keys:owned});
        history.evict_above(&mut rows,Some(tail_key),true);
        assert_eq!(rows.len(),1);assert_eq!(rows[0].ui_key,tail_key);assert_eq!(rows[0].tool_output.as_deref(),Some("keep tail result"));
        assert_eq!(history.remaining(),12);
    }
    use super::*;
    use feather_core::message::{Api,ContentBlock,Message,StopReason,Usage};
    #[test]
    fn resume_compacted_context_pages_originals_without_a_summary_bubble() {
        let home=std::env::temp_dir().join(feather_core::provider::gen_id("history-test"));
        let root=home.join("workspace");std::fs::create_dir_all(&root).unwrap();
        let mut session=Session::new(&home,&root).unwrap();
        for n in 0..250 {session.push(Message::user_text(format!("original prompt {n}"))).unwrap();}
        let summary=feather_core::compaction::summary_message("PRIVATE SUMMARY",&[],&[]);
        session.set_messages_for_compaction(vec![summary]);session.commit_turn().unwrap();
        let dir=session.dir().to_path_buf();drop(session);
        let session=Session::open(&dir,&home).unwrap();
        assert!(session.messages()[0].text().contains("PRIVATE SUMMARY"));
        let (mut history,mut rows)=History::from_session(&session).unwrap();
        assert_eq!(rows.len(),PAGE_ROWS);assert_eq!(rows[0].text,"original prompt 154");
        let ctx=eframe::egui::Context::default();
        while history.remaining()>0 {
            let count=rows.len();history.request_older(&ctx);
            let started=std::time::Instant::now();
            while history.loading(){history.poll(&mut rows);assert!(started.elapsed()<std::time::Duration::from_secs(3));std::thread::sleep(std::time::Duration::from_millis(2));}
            assert!(rows.len()>count);
        }
        assert_eq!(rows.len(),250);
        for(n,row) in rows.iter().enumerate(){assert_eq!(row.text,format!("original prompt {n}"));}
        assert!(!rows.iter().any(|row|row.text.contains("PRIVATE SUMMARY")));
        drop(history);drop(session);std::fs::remove_dir_all(home).unwrap();
    }
    #[test]
    fn evicting_loaded_pages_restores_cursor_and_reloads_exact_sequence() {
        let home=std::env::temp_dir().join(feather_core::provider::gen_id("history-evict-test"));
        let root=home.join("workspace");std::fs::create_dir_all(&root).unwrap();
        let mut session=Session::new(&home,&root).unwrap();
        for n in 0..250 {session.push(Message::user_text(format!("evict prompt {n}"))).unwrap();}
        let (mut history,mut rows)=History::from_session(&session).unwrap();
        let ctx=eframe::egui::Context::default();
        while history.remaining()>0 {
            history.request_older(&ctx);
            let started=std::time::Instant::now();
            while history.loading(){history.poll(&mut rows);assert!(started.elapsed()<std::time::Duration::from_secs(3));std::thread::sleep(std::time::Duration::from_millis(2));}
        }
        assert_eq!(rows.len(),250);assert_eq!(history.remaining(),0);
        assert_eq!(history.evict_above(&mut rows,None,true),154);
        assert_eq!(rows.len(),PAGE_ROWS);assert_eq!(rows[0].text,"evict prompt 154");
        assert!(history.remaining()>0);
        while history.remaining()>0 {
            history.request_older(&ctx);
            let started=std::time::Instant::now();
            while history.loading(){history.poll(&mut rows);assert!(started.elapsed()<std::time::Duration::from_secs(3));std::thread::sleep(std::time::Duration::from_millis(2));}
        }
        assert_eq!(rows.len(),250);
        for(n,row) in rows.iter().enumerate(){assert_eq!(row.text,format!("evict prompt {n}"));}
        drop(history);drop(session);std::fs::remove_dir_all(home).unwrap();
    }
    #[test]
    fn evict_above_keeps_two_page_buffer_until_bottom() {
        let mut rows:Vec<_>=(0..20).map(|n|ChatRow::new(crate::RowKind::User,format!("buffer {n}"))).collect();
        let groups:Vec<LoadedPage>=rows.chunks(5).take(3).enumerate().map(|(i,chunk)|LoadedPage{before:Some(((i+1)*5) as u64),keys:chunk.iter().map(|row|row.ui_key).collect()}).collect();
        let tail_key=rows[15].ui_key;
        let mut history=History{store:None,before:None,tail_before:Some(15),loaded:std::collections::VecDeque::from(groups),pending:None,tail_pending:None,committed_before:None,committed_key:None,tail_refresh_ready:false,error:None};
        assert_eq!(history.evict_above(&mut rows,Some(tail_key),false),5);
        assert_eq!(rows.len(),15);assert_eq!(rows[0].text,"buffer 5");assert_eq!(history.remaining(),5);
        assert_eq!(history.evict_above(&mut rows,None,true),10);
        assert_eq!(rows.len(),5);assert_eq!(rows[0].text,"buffer 15");assert_eq!(history.remaining(),15);
    }
    #[test]
    fn paging_across_tool_call_result_boundary_joins_one_original_card() {
        let call=feather_core::message::ToolCall{id:"tool-id".into(),name:"read".into(),arguments:serde_json::json!({"path":"source.rs"})};
        let old=Message::assistant(vec![ContentBlock::ToolCall(call)],"synthetic",Api::Faux,Usage::default(),StopReason::ToolUse);
        let result=Message::tool_result("tool-id","read","original result",false);
        let mut rows=rows_from_messages(&[result]);
        let result_key=rows[0].ui_key;
        let keys=prepend(rows_from_messages(&[old]),&mut rows);
        assert_eq!(rows.len(),1);assert_eq!(rows[0].tool_output.as_deref(),Some("original result"));
        assert!(keys.is_empty(),"tail identity remains owned by its original page");assert_eq!(rows[0].ui_key,result_key);
        assert!(!rows[0].tool_args.is_empty());
    }
    #[test]
    fn switching_history_while_reading_cannot_attach_an_old_session_page() {
        let home=std::env::temp_dir().join(feather_core::provider::gen_id("history-switch-test"));
        let root=home.join("root");std::fs::create_dir_all(&root).unwrap();
        let mut a=Session::new(&home,&root).unwrap();
        for n in 0..110{a.push(Message::user_text(format!("session-A {n}"))).unwrap();}
        let (mut history,_)=History::from_session(&a).unwrap();history.request_older(&eframe::egui::Context::default());
        let mut b=Session::new(&home,&root).unwrap();b.push(Message::user_text("session-B only")).unwrap();
        let (fresh,mut rows)=History::from_session(&b).unwrap();history=fresh;
        assert!(!history.poll(&mut rows));assert_eq!(rows.len(),1);assert_eq!(rows[0].text,"session-B only");
        drop(history);drop(a);drop(b);std::thread::sleep(std::time::Duration::from_millis(50));std::fs::remove_dir_all(home).unwrap();
    }
}
