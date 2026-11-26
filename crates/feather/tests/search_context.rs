//! End-to-end checks for the `search_context` tool.
//!
//! The point of the tool is that compaction is lossy while storage is not, so
//! these tests write real messages into a real `session.redb` and read them back
//! through the same entry point the model uses. A test that only exercised the
//! snippet function would not prove the tool can reach history at all.

use feather::ctxsearch::SearchContextTool;
use feather_core::message::Message;
use feather_core::session_store::SessionStore;
use feather_core::tool::AgentTool;

/// A plain assistant text row.
fn assistant(text: &str) -> Message {
    Message::assistant(
        vec![feather_core::message::ContentBlock::Text {
            text: text.to_string(),
        }],
        "m",
        feather_core::message::Api::Faux,
        feather_core::message::Usage::default(),
        feather_core::message::StopReason::Stop,
    )
}

fn temp_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "feather-ctxsearch-{tag}-{}-{}",
        std::process::id(),
        feather_core::message::now_millis()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Write a handful of messages the way a real session would.
fn seed(dir: &std::path::Path) -> SessionStore {
    let store = SessionStore::open(dir).unwrap();
    let base = 1_700_000_000_000u64;
    let msgs = vec![
        Message::user_text("please fix the parser in crates/feather-core/src/parser.rs"),
        assistant("reading the parser now"),
        Message::tool_result(
            "c1",
            "bash",
            "error[E0308]: mismatched types at src/parser.rs:412",
            true,
        ),
        Message::user_text("unrelated chatter about nothing in particular"),
        assistant("the fix is a type annotation"),
    ];
    for (i, m) in msgs.iter().enumerate() {
        store
            .append_raw(&format!("id-{i}"), base + i as u64, m)
            .unwrap();
    }
    store
}

#[tokio::test]
async fn finds_a_detail_from_earlier_in_the_session() {
    let dir = temp_dir("finds");
    let _store = seed(&dir);
    let tool = SearchContextTool {
        session_dir: dir.clone(),
    };
    let out = tool
        .execute(
            serde_json::json!({ "query": "E0308" }),
            tokio::sync::watch::channel(false).1,
        )
        .await;
    assert!(!out.is_error, "{}", out.text);
    assert!(out.text.contains("E0308"), "{}", out.text);
    assert!(out.text.contains("parser.rs:412"), "{}", out.text);
    assert!(
        out.text.contains("tool"),
        "role should be reported: {}",
        out.text
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn search_is_case_insensitive() {
    let dir = temp_dir("case");
    let _store = seed(&dir);
    let tool = SearchContextTool {
        session_dir: dir.clone(),
    };
    for q in ["e0308", "E0308", "E0308".to_lowercase().as_str()] {
        let out = tool
            .execute(
                serde_json::json!({ "query": q }),
                tokio::sync::watch::channel(false).1,
            )
            .await;
        assert!(out.text.contains("E0308"), "query {q}: {}", out.text);
    }
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn missing_query_is_an_error_not_a_panic() {
    let dir = temp_dir("missing");
    let _store = seed(&dir);
    let tool = SearchContextTool {
        session_dir: dir.clone(),
    };
    for args in [
        serde_json::json!({}),
        serde_json::json!({ "query": "" }),
        serde_json::json!({ "query": "   " }),
        serde_json::json!({ "query": 42 }),
    ] {
        let out = tool
            .execute(args.clone(), tokio::sync::watch::channel(false).1)
            .await;
        assert!(out.is_error, "expected error for {args}: {}", out.text);
    }
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn a_query_with_no_match_says_so_and_reports_what_it_scanned() {
    let dir = temp_dir("nomatch");
    let _store = seed(&dir);
    let tool = SearchContextTool {
        session_dir: dir.clone(),
    };
    let out = tool
        .execute(
            serde_json::json!({ "query": "zzz-not-present-zzz" }),
            tokio::sync::watch::channel(false).1,
        )
        .await;
    assert!(!out.is_error, "{}", out.text);
    assert!(out.text.contains("no match"), "{}", out.text);
    assert!(out.text.contains("stored messages"), "{}", out.text);
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn role_filter_narrows_results() {
    let dir = temp_dir("role");
    let _store = seed(&dir);
    let tool = SearchContextTool {
        session_dir: dir.clone(),
    };
    // "parser" appears in a user message and in a tool result.
    let all = tool
        .execute(
            serde_json::json!({ "query": "parser" }),
            tokio::sync::watch::channel(false).1,
        )
        .await;
    let only_tool = tool
        .execute(
            serde_json::json!({ "query": "parser", "role": "tool" }),
            tokio::sync::watch::channel(false).1,
        )
        .await;
    assert!(all.text.contains("user"), "{}", all.text);
    assert!(all.text.contains("tool"), "{}", all.text);
    assert!(!only_tool.text.contains("· user"), "{}", only_tool.text);
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn limit_bounds_the_result_rows() {
    let dir = temp_dir("limit");
    let _store = seed(&dir);
    let tool = SearchContextTool {
        session_dir: dir.clone(),
    };
    let out = tool
        .execute(
            serde_json::json!({ "query": "the", "limit": 1 }),
            tokio::sync::watch::channel(false).1,
        )
        .await;
    let rows = out.text.matches("[seq ").count();
    assert_eq!(rows, 1, "limit must cap rows: {}", out.text);
    std::fs::remove_dir_all(&dir).ok();
}

/// A `limit` stop in the middle of a page must still say more rows exist.
///
/// The first version reported "no more" whenever the page had no older sibling,
/// which is wrong: rows after the stop point on the same page were never
/// examined. This seeds many matches inside a single page and asks for one.
#[tokio::test]
async fn a_limit_stop_mid_page_still_reports_more_may_exist() {
    use feather_core::ctxsearch::{search_store, RoleFilter};
    let dir = temp_dir("midtpage");
    let store = SessionStore::open(&dir).unwrap();
    // Many matches, all inside one raw page (RAW_PAGE_MAX_ROWS = 256).
    for i in 0..40 {
        let msg = Message::user_text(format!("alpha {i} common-token here"));
        store
            .append_raw(&format!("mid-{i}"), 1_700_000_000_000 + i, &msg)
            .unwrap();
    }
    let found = search_store(&store, "common-token", RoleFilter::Any, 1, 2000).unwrap();
    assert_eq!(found.hits.len(), 1, "limit must cap hits");
    assert!(
        found.more_may_exist,
        "39 matches were never examined and are on the same page: {found:?}"
    );
    drop(store);
    std::fs::remove_dir_all(&dir).ok();
}

/// When the last matching row is the end of the audit, saying "no more" is right.
#[tokio::test]
async fn an_exhausted_scan_reports_nothing_more() {
    use feather_core::ctxsearch::{search_store, RoleFilter};
    let dir = temp_dir("exhausted");
    let store = SessionStore::open(&dir).unwrap();
    for i in 0..5 {
        store
            .append_raw(
                &format!("e-{i}"),
                1_700_000_000_000 + i,
                &Message::user_text(format!("body {i}")),
            )
            .unwrap();
    }
    store
        .append_raw(
            "last",
            1_700_000_000_100,
            &Message::user_text("the unique-tail-token"),
        )
        .unwrap();
    let found = search_store(&store, "unique-tail-token", RoleFilter::Any, 20, 2000).unwrap();
    assert_eq!(found.hits.len(), 1, "{found:?}");
    assert!(
        !found.more_may_exist,
        "the scan reached the start and still had room: {found:?}"
    );
    drop(store);
    std::fs::remove_dir_all(&dir).ok();
}
/// Searching the session that is currently open must work, not hit the lock.
///
/// This is the case the tool exists for: the live session is the one that was
/// just compacted. redb allows one open handle per file per process, so a search
/// that opened its own handle would fail exactly where it is most needed. The
/// store is process-cached, so a search issued while the session is open has to
/// reuse that handle.
#[tokio::test]
async fn searching_while_the_session_is_open_reuses_the_handle() {
    use feather_core::ctxsearch::{search_store, RoleFilter};
    let dir = temp_dir("open");
    // The writer holds the handle for the whole test, as a live session does.
    let writer = SessionStore::open(&dir).unwrap();
    writer
        .append_raw(
            "c1",
            feather_core::message::now_millis(),
            &Message::user_text("live-session-token is here"),
        )
        .unwrap();

    // A separate open must resolve to the same process-wide handle rather than
    // failing with "Database already open".
    let reader = SessionStore::open(&dir).expect("second open must reuse the handle");
    let found = search_store(&reader, "live-session-token", RoleFilter::Any, 5, 100)
        .expect("search on an open session");
    assert_eq!(found.hits.len(), 1, "{found:?}");

    // And through the tool path, which opens by directory.
    let tool = SearchContextTool {
        session_dir: dir.clone(),
    };
    let out = tool
        .execute(
            serde_json::json!({ "query": "live-session-token" }),
            tokio::sync::watch::channel(false).1,
        )
        .await;
    assert!(
        !out.is_error,
        "tool must read an open session: {}",
        out.text
    );
    assert!(out.text.contains("live-session-token"), "{}", out.text);

    drop(reader);
    drop(writer);
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn searching_a_session_with_no_store_does_not_create_one() {
    let dir = temp_dir("nostore");
    // Deliberately never seeded.
    let tool = SearchContextTool {
        session_dir: dir.clone(),
    };
    let out = tool
        .execute(
            serde_json::json!({ "query": "anything" }),
            tokio::sync::watch::channel(false).1,
        )
        .await;
    assert!(out.is_error, "{}", out.text);
    assert!(
        !dir.join("session.redb").exists(),
        "a read-only search must not materialize a session store"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn the_tool_advertises_itself_consistently() {
    let tool = SearchContextTool {
        session_dir: std::env::temp_dir(),
    };
    assert_eq!(tool.name(), "search_context");
    assert!(tool.parallel_safe(), "searching is read-only");
    let params = tool.parameters();
    assert_eq!(params["required"], serde_json::json!(["query"]));
    assert!(
        tool.description().contains("compaction"),
        "the description must explain it reaches folded turns: {}",
        tool.description()
    );
}
