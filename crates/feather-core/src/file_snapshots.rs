//! Content-addressed, session-scoped file history for anchor-tool mutations.
//! Captures the pre-edit baseline, including absence, and committed turn states.
//! Absolute paths permit external edits; restore refuses changed files/symlinks.
//! Git workspace history remains available for legacy and shell-made changes.
use crate::records::Records;
use redb::{ReadableTable, TableDefinition};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, fs, io, path::{Path,PathBuf}};
const META: TableDefinition<&str, &[u8]> = TableDefinition::new("file_history");
const BLOBS: TableDefinition<&str, &[u8]> = TableDefinition::new("file_blobs");
const MAX_FILE: u64 = 16 * 1024 * 1024;
#[derive(Clone,Debug,PartialEq,Eq,Serialize,Deserialize)]
struct State { hash: Option<String>, readonly: bool, #[serde(default)] mode: Option<u32> }
#[derive(Clone,Serialize,Deserialize)]
struct History { baseline: State, versions: BTreeMap<u64,State>, current: State }
#[derive(Default,Serialize,Deserialize)]
struct Metadata { turn:u64, paths:BTreeMap<String,History> }
/// Resolves existing parent components without following a final symlink.
fn identity(path:&Path)->io::Result<PathBuf> {
    let absolute=if path.is_absolute(){path.to_path_buf()}else{std::env::current_dir()?.join(path)};
    if let Ok(m)=fs::symlink_metadata(&absolute) {
        if m.file_type().is_symlink() || !m.is_file() {return Err(io::Error::other("file history refuses symlinks/directories"));}
        return fs::canonicalize(absolute);
    }
    let mut tail=Vec::new();let mut parent=absolute.as_path();
    while !parent.exists() {
        tail.push(parent.file_name().ok_or_else(||io::Error::other("invalid file path"))?.to_os_string());
        parent=parent.parent().ok_or_else(||io::Error::other("file has no existing ancestor"))?;
    }
    let mut resolved=fs::canonicalize(parent)?;
    for part in tail.into_iter().rev(){resolved.push(part);}
    Ok(resolved)
}
fn capture(path:&Path)->io::Result<(State,Vec<u8>)> {
    let meta=match fs::symlink_metadata(path) {
        Ok(m)=>m,Err(e) if e.kind()==io::ErrorKind::NotFound=>return Ok((State{hash:None,readonly:false,mode:None},Vec::new())),Err(e)=>return Err(e),
    };
    if !meta.is_file() || meta.file_type().is_symlink() {return Err(io::Error::other("file history refuses symlinks/directories"));}
    if meta.len()>MAX_FILE{return Err(io::Error::other("file history limit is 16 MiB per file"));}
    let bytes=fs::read(path)?;
    if bytes.len() as u64>MAX_FILE{return Err(io::Error::other("file grew beyond snapshot limit"));}
    #[cfg(unix)] let mode={use std::os::unix::fs::PermissionsExt;Some(meta.permissions().mode())};
    #[cfg(not(unix))] let mode=None;
    Ok((State{hash:Some(format!("{:x}",Sha256::digest(&bytes))),readonly:meta.permissions().readonly(),mode},bytes))
}
fn put_blob(table:&mut redb::Table<&str,&[u8]>,state:&State,bytes:&[u8])->io::Result<()> {
    if let Some(hash)=&state.hash {
        let exists=table.get(hash.as_str()).map_err(io::Error::other)?.is_some();
        if !exists{table.insert(hash.as_str(),bytes).map_err(io::Error::other)?;}
    } Ok(())
}
fn read_meta(db:&redb::Database)->io::Result<Metadata> {
    let tx=db.begin_read().map_err(io::Error::other)?;
    match tx.open_table(META) {
        Ok(table)=>table.get("state").map_err(io::Error::other)?.map(|v|serde_json::from_slice(v.value()).map_err(io::Error::other)).transpose().map(|v|v.unwrap_or_default()),
        Err(redb::TableError::TableDoesNotExist(_))=>Ok(Metadata::default()),
        Err(e)=>Err(io::Error::other(e)),
    }
}
fn save_meta(db:&redb::Database,meta:&Metadata,blobs:&[(State,Vec<u8>)])->io::Result<()> {
    let tx=db.begin_write().map_err(io::Error::other)?;
    {
        let mut table=tx.open_table(BLOBS).map_err(io::Error::other)?;
        for (state,bytes) in blobs{put_blob(&mut table,state,bytes)?;}
        tx.open_table(META).map_err(io::Error::other)?.insert("state",serde_json::to_vec(meta).map_err(io::Error::other)?.as_slice()).map_err(io::Error::other)?;
    }
    tx.commit().map_err(io::Error::other)
}
#[derive(Clone)]
pub struct FileSnapshots { dir:PathBuf }
impl FileSnapshots {
    pub fn new(session_dir:&Path)->Self {Self{dir:session_dir.to_path_buf()}}
    fn store(&self)->io::Result<Records>{Records::open(&self.dir.join("checkpoints.redb")).map_err(io::Error::other)}
    /// Must succeed before the file is mutated. An existing tracked file's
    /// externally changed content becomes the expected pre-edit state.
    pub fn before_edit(&self,path:&Path)->io::Result<()> {
        let path=identity(path)?;
        let key=path.to_str().ok_or_else(||io::Error::other("snapshot paths must be UTF-8"))?.to_string();
        let (state,bytes)=capture(&path)?;
        let store=self.store()?;
        let _guard=EDIT_LOCK.lock().unwrap_or_else(|e|e.into_inner());
        let mut meta=read_meta(store.database())?;
        if let Some(h)=meta.paths.get_mut(&key){h.current=state.clone();}
        else{meta.paths.insert(key,History{baseline:state.clone(),versions:BTreeMap::new(),current:state.clone()});}
        save_meta(store.database(),&meta,&[(state,bytes)])
    }
    /// Record the expected on-disk post-edit state, also during interrupted turns.
    pub fn after_edit(&self,path:&Path)->io::Result<()> {
        let path=identity(path)?;let key=path.to_str().ok_or_else(||io::Error::other("invalid path"))?;
        let (state,bytes)=capture(&path)?;let store=self.store()?;
        let _guard=EDIT_LOCK.lock().unwrap_or_else(|e|e.into_inner());
        let mut meta=read_meta(store.database())?;
        let history=meta.paths.get_mut(key).ok_or_else(||io::Error::other("missing pre-edit baseline"))?;
        history.current=state.clone();save_meta(store.database(),&meta,&[(state,bytes)])
    }
    pub fn commit(&self,turn:u64)->io::Result<()> {
        let store=self.store()?;let _guard=EDIT_LOCK.lock().unwrap_or_else(|e|e.into_inner());
        let mut meta=read_meta(store.database())?;
        let tx=store.database().begin_write().map_err(io::Error::other)?;
        {
            let mut blobs=tx.open_table(BLOBS).map_err(io::Error::other)?;
            for (path,h) in &mut meta.paths {
                let (state,bytes)=capture(Path::new(path))?;
                if state != h.current {
                    return Err(io::Error::other(format!("snapshot conflict: {path} changed after the last tool edit")));
                }
                // A new prompt after rewind discards divergent future versions.
                h.versions.retain(|t,_|*t<=meta.turn);
                h.versions.insert(turn,state.clone());h.current=state.clone();
                put_blob(&mut blobs,&state,&bytes)?;
            }
            meta.turn=turn;
            tx.open_table(META).map_err(io::Error::other)?.insert("state",serde_json::to_vec(&meta).map_err(io::Error::other)?.as_slice()).map_err(io::Error::other)?;
        }
        tx.commit().map_err(io::Error::other)
    }
    /// Verify *all* files before restoring any. Avoids silently overwriting edits
    /// by the user/another session after a turn. Individual replacements are
    /// atomic; failures roll back already-applied replacements where possible.
    pub fn restore(&self,turn:u64)->io::Result<()> {
        self.restore_with(turn, || Ok(()))
    }
    pub fn restore_with(&self, turn:u64, restore_workspace:impl FnOnce()->io::Result<()>) -> io::Result<()> {
        let store=self.store()?;let _guard=EDIT_LOCK.lock().unwrap_or_else(|e|e.into_inner());
        let mut meta=read_meta(store.database())?;
        let tx=store.database().begin_read().map_err(io::Error::other)?;
        let blobs=match tx.open_table(BLOBS){Ok(t)=>t,Err(redb::TableError::TableDoesNotExist(_))=>return restore_workspace(),Err(e)=>return Err(io::Error::other(e))};
        // Only metadata stays in memory: fetch one blob at a time for writes.
        for (path,h) in &meta.paths {
            if identity(Path::new(path))?!=Path::new(path){return Err(io::Error::other("snapshot path now resolves elsewhere"));}
            if capture(Path::new(path))?.0!=h.current{return Err(io::Error::other(format!("rewind conflict: {path} changed outside this session")));}
        }
        let mut applied:Vec<String>=Vec::new();
        for (path,h) in &meta.paths {
            let target=h.versions.range(..=turn).next_back().map(|(_,s)|s).unwrap_or(&h.baseline);
            if &h.current==target {continue;}
            if let Err(e)=restore_state(Path::new(path),target,&blobs) {
                for key in applied.iter().rev(){let _=restore_state(Path::new(key),&meta.paths[key].current,&blobs);}
                return Err(e);
            }
            applied.push(path.clone());
        }
        if let Err(e)=restore_workspace() {
            for key in applied.iter().rev(){let _=restore_state(Path::new(key),&meta.paths[key].current,&blobs);}
            return Err(e);
        }
        for h in meta.paths.values_mut(){h.current=h.versions.range(..=turn).next_back().map(|(_,s)|s).unwrap_or(&h.baseline).clone();}
        meta.turn=turn;drop(blobs);drop(tx);
        save_meta(store.database(),&meta,&[])
    }
    /// Fork inherits committed file history, not the source's active edits.
    pub fn fork_into(&self,destination:&Path)->io::Result<()> {
        let store=self.store()?;let tx=store.database().begin_read().map_err(io::Error::other)?;
        let meta=read_meta(store.database())?;
        if meta.paths.is_empty(){return Ok(());}
        let target=FileSnapshots::new(destination).store()?;
        let write=target.database().begin_write().map_err(io::Error::other)?;
        {
            let blobs=tx.open_table(BLOBS).map_err(io::Error::other)?;
            let mut dest=write.open_table(BLOBS).map_err(io::Error::other)?;
            for row in blobs.iter().map_err(io::Error::other)? {
                let (key,value)=row.map_err(io::Error::other)?;dest.insert(key.value(),value.value()).map_err(io::Error::other)?;
            }
            write.open_table(META).map_err(io::Error::other)?.insert("state",serde_json::to_vec(&meta).map_err(io::Error::other)?.as_slice()).map_err(io::Error::other)?;
        }
        write.commit().map_err(io::Error::other)
    }
    pub fn has_history(&self)->io::Result<bool>{let store=self.store()?;Ok(!read_meta(store.database())?.paths.is_empty())}
}
static EDIT_LOCK:std::sync::Mutex<()>=std::sync::Mutex::new(());
fn restore_state(path:&Path,state:&State,blobs:&impl ReadableTable<&'static str,&'static [u8]>)->io::Result<()> {
    if state.hash.is_none(){return match fs::remove_file(path){Ok(())=>Ok(()),Err(e) if e.kind()==io::ErrorKind::NotFound=>Ok(()),Err(e)=>Err(e)};}
    let hash=state.hash.as_ref().unwrap();
    let data=blobs.get(hash.as_str()).map_err(io::Error::other)?.ok_or_else(||io::Error::other("file snapshot blob missing"))?;
    if format!("{:x}",Sha256::digest(data.value()))!=*hash{return Err(io::Error::other("file snapshot blob corrupt"));}
    let parent=path.parent().ok_or_else(||io::Error::other("file has no parent"))?;fs::create_dir_all(parent)?;
    let temp=parent.join(format!(".feather-restore-{}",crate::provider::gen_id("tmp")));
    let result=(||{
        use std::io::Write;
        let mut file=fs::OpenOptions::new().create_new(true).write(true).open(&temp)?;
        file.write_all(data.value())?;file.sync_all()?;drop(file);
        let mut permissions=fs::metadata(&temp)?.permissions();
        #[cfg(unix)] if let Some(mode)=state.mode {use std::os::unix::fs::PermissionsExt;permissions.set_mode(mode);}
        permissions.set_readonly(state.readonly);fs::set_permissions(&temp,permissions)?;
        fs::rename(&temp,path)
    })();
    if result.is_err(){let _=fs::remove_file(&temp);}result
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn external_rewind_baselines_deletion_forward_and_conflict() {
        let root=std::env::temp_dir().join(crate::provider::gen_id("file-snapshot"));fs::create_dir_all(&root).unwrap();
        let existing=root.join("outside.txt");let created=root.join("new.txt");fs::write(&existing,b"original\r\n").unwrap();
        let store=FileSnapshots::new(&root.join("session"));
        store.commit(1).unwrap();
        store.before_edit(&existing).unwrap();fs::write(&existing,b"changed").unwrap();store.after_edit(&existing).unwrap();
        store.before_edit(&created).unwrap();fs::write(&created,b"created").unwrap();store.after_edit(&created).unwrap();store.commit(2).unwrap();
        store.restore(1).unwrap();assert_eq!(fs::read(&existing).unwrap(),b"original\r\n");assert!(!created.exists());
        store.restore(2).unwrap();assert_eq!(fs::read(&created).unwrap(),b"created");
        fs::write(&existing,b"user change").unwrap();assert!(store.restore(1).is_err());assert!(created.exists());
        assert_eq!(fs::read(&existing).unwrap(),b"user change");fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn reopen_branch_and_failed_workspace_restore_are_safe() {
        let root=std::env::temp_dir().join(crate::provider::gen_id("snapshot-branch"));fs::create_dir_all(&root).unwrap();
        let path=root.join("external.txt");fs::write(&path,b"base").unwrap();
        let s=FileSnapshots::new(&root.join("session"));s.commit(1).unwrap();
        s.before_edit(&path).unwrap();fs::write(&path,b"two").unwrap();s.after_edit(&path).unwrap();s.commit(2).unwrap();
        s.before_edit(&path).unwrap();fs::write(&path,b"three").unwrap();s.after_edit(&path).unwrap();s.commit(3).unwrap();
        assert!(s.restore_with(1,||Err(io::Error::other("workspace restore failed"))).is_err());
        assert_eq!(fs::read(&path).unwrap(),b"three");
        drop(s);let reopened=FileSnapshots::new(&root.join("session"));reopened.restore(1).unwrap();
        reopened.before_edit(&path).unwrap();fs::write(&path,b"new two").unwrap();reopened.after_edit(&path).unwrap();reopened.commit(2).unwrap();
        reopened.restore(3).unwrap();assert_eq!(fs::read(&path).unwrap(),b"new two","discarded future must not reappear");
        fs::remove_dir_all(root).unwrap();
    }
    #[cfg(unix)]
    #[test]
    fn symlink_replacement_is_rejected() {
        let root=std::env::temp_dir().join(crate::provider::gen_id("snapshot-symlink"));fs::create_dir_all(&root).unwrap();
        let path=root.join("file");let victim=root.join("victim");fs::write(&path,b"base").unwrap();fs::write(&victim,b"user").unwrap();
        let s=FileSnapshots::new(&root.join("session"));s.before_edit(&path).unwrap();fs::write(&path,b"edited").unwrap();s.after_edit(&path).unwrap();s.commit(1).unwrap();
        fs::remove_file(&path).unwrap();std::os::unix::fs::symlink(&victim,&path).unwrap();
        assert!(s.restore(0).is_err());assert_eq!(fs::read(&victim).unwrap(),b"user");fs::remove_dir_all(root).unwrap();
    }
}
