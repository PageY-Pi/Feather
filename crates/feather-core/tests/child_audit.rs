use feather_core::{substore::SubagentStore, Message, Session};
use std::sync::Arc;
#[test]
fn child_compaction_never_overwrites_original_audit() {
    let root = std::env::temp_dir().join(feather_core::provider::gen_id("child-audit"));
    let store = Arc::new(SubagentStore::open(&root).unwrap());
    store.start("child", "audit", "task", "synthetic").unwrap();
    let mut session = Session::child(&root, "child".into(), store.clone());
    session.push(Message::user_text("original one")).unwrap();
    session.push(Message::user_text("original two")).unwrap();
    session.set_messages_for_compaction(vec![Message::user_text("summary")]);
    session
        .push(Message::user_text("after compaction"))
        .unwrap();
    let messages = store.messages("child").unwrap();
    assert_eq!(
        messages.iter().map(Message::text).collect::<Vec<_>>(),
        vec!["original one", "original two", "after compaction"]
    );
    assert_eq!(store.list().unwrap()[0].lines, 3);
    drop(session);
    drop(store);
    std::fs::remove_dir_all(root).unwrap();
}
