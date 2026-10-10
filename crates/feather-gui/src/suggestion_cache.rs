//! Slash-list memoization: repeated egui layout passes borrow one list instead
//! of formatting/allocating all descriptions twice per frame.
use std::cell::{Ref,RefCell};
pub type Suggestion=(String,String,bool);
#[derive(Default)]
struct Cached {input:String,skills:u64,turns:u64,ready:bool,rows:Vec<Suggestion>}
#[derive(Default)]
pub struct Suggestions(RefCell<Cached>);
impl Suggestions {
    pub fn get(&self,input:&str,skills:u64,turns:u64,build:impl FnOnce()->Vec<Suggestion>)->Ref<'_,[Suggestion]> {
        {
            let mut cache=self.0.borrow_mut();
            if !cache.ready||cache.input!=input||cache.skills!=skills||cache.turns!=turns {
                cache.input.clear();cache.input.push_str(input);cache.skills=skills;cache.turns=turns;
                cache.rows=build();cache.ready=true;
            }
        }
        Ref::map(self.0.borrow(),|cache|cache.rows.as_slice())
    }
}
#[cfg(test)]mod tests {
    use super::*;
    #[test]fn stable_input_and_metadata_build_once_not_per_frame(){
        let cache=Suggestions::default();let count=std::cell::Cell::new(0);
        for _ in 0..100 {let rows=cache.get("/skill ",1,0,||{count.set(count.get()+1);vec![("/skill:docx ".into(),"Word documents".into(),false)]});assert_eq!(rows.len(),1);}
        assert_eq!(count.get(),1);
        drop(cache.get("/skill ",2,0,||{count.set(count.get()+1);Vec::new()}));assert_eq!(count.get(),2);
    }
}
