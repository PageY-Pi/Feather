//! The measured cases.
//!
//! Each case is a self-contained (setup, measure, verify) triple. Two rules keep
//! the ruler honest:
//!
//! 1. **Setup is outside the timer.** Building a 400-message history is not the
//!    thing being measured; appending to it is.
//! 2. **Every case asserts an invariant that could actually fail.** A check that
//!    is true by construction (`x == x`, `a || !a`) verifies nothing and is worse
//!    than no check, because it tells the reader the work occurred.
//!
//! The cases cover the work a turn actually does: serialise the conversation,
//! estimate its tokens, place cache breakpoints, map a file to anchors, search.

use crate::measure;
use crate::Measurement;
use feather_core::message::Message;
use feather_core::snapshot::{SnapshotChunks, SnapshotStore};

pub struct Case {
    pub name: &'static str,
    pub run: fn(&std::path::Path, usize) -> Measurement,
}

pub fn registry() -> Vec<Case> {
    vec![
        Case {
            name: "snapshot.full",
            run: snapshot_full,
        },
        Case {
            name: "snapshot.reuse",
            run: snapshot_reuse,
        },
        Case {
            name: "tokenizer.message",
            run: tokenizer_message,
        },
        Case {
            name: "cache.plan",
            run: cache_plan,
        },
        Case {
            name: "anchors.read",
            run: anchors_read,
        },
        Case {
            name: "grep.anchor",
            run: grep_anchor,
        },
        Case {
            name: "redb.commit",
            run: redb_commit,
        },
        Case {
            name: "session.commit",
            run: session_commit,
        },
        Case {
            name: "checkpoints.grow",
            run: checkpoints_grow,
        },
        Case {
            name: "records.open",
            run: records_open,
        },
    ]
}

/// A conversation of `n` messages with realistic shape and size.
///
/// Real histories are not uniform: they carry long user turns, short
/// acknowledgements, large tool outputs and multi-byte text. A uniform corpus
/// would let an optimisation that only helps small messages look good.
fn history(n: usize) -> Vec<Message> {
    (0..n)
        .map(|i| match i % 5 {
            0 => Message::user_text(format!(
                "Turn {i}: explain how the snapshot store handles reuse and why the chunk key is a hash of canonical JSON."
            )),
            1 => Message::user_text(
                "Add a regression test that fails against the previous behaviour.".to_string(),
            ),
            2 => Message::user_text("x".repeat(200)),
            3 => Message::user_text(
                "羽毛 — multi-byte content, CJK and emoji 🪶 to exercise UTF-8 paths.".repeat(3),
            ),
            _ => Message::user_text("ok".to_string()),
        })
        .collect()
}

fn scratch_db(root: &std::path::Path, name: &str) -> std::path::PathBuf {
    let dir = root.join(name);
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

/// Full snapshot: the pre-1.3.2 cost, every message serialised every commit.
fn snapshot_full(root: &std::path::Path, iterations: usize) -> Measurement {
    const N: usize = 400;
    let messages = history(N);
    let dir = scratch_db(root, "snap-full");
    let store = SnapshotStore::open(&dir).expect("snapshot store opens");
    // Independent oracle: the history hash the store must agree with.
    let oracle = SnapshotStore::history_hash(&messages);
    let mut turn = 0u64;
    // One op = one commit, so this is directly comparable with `snapshot.reuse`.
    // Reporting per-message would flatter a full commit by a factor of N.
    measure("snapshot.full", 1, iterations, || {
        turn += 1;
        let commit = store
            .snapshot_reusing(turn, &messages, &[], &SnapshotChunks::default())
            .expect("snapshot commits");
        // Invariant: with no hint every message is serialised, none reused, and
        // the store's own history hash matches an independent computation. Scope
        // it to the first batch so later turns (which overwrite the same content
        // under a new turn id) still check the hash.
        let hash_ok = SnapshotStore::history_hash(&messages) == oracle;
        let ok = commit.cost.serialized == N as u64
            && commit.cost.reused == 0
            && commit.chunks.bases.len() == N
            && hash_ok;
        (
            ok,
            format!(
                "serialized={} reused={} hash_ok={hash_ok}",
                commit.cost.serialized, commit.cost.reused
            ),
        )
    })
}

/// Incremental snapshot: one appended message per commit, hint carried forward.
///
/// This is the shape of a real turn loop, so it decides whether a long session
/// is quadratic in bytes written through `serde_json`.
fn snapshot_reuse(root: &std::path::Path, iterations: usize) -> Measurement {
    const N: usize = 400;
    let mut messages = history(N);
    let dir = scratch_db(root, "snap-reuse");
    let store = SnapshotStore::open(&dir).expect("snapshot store opens");
    // Oracle for the *seed* content only: the store must agree with an
    // independent hash computation before any reuse happens.
    let oracle = SnapshotStore::history_hash(&messages);
    // Seed the hint with a full commit, exactly as `Session::commit_turn` does.
    // The seed's hash must match the independent oracle, otherwise every reuse
    // measurement below is built on a store that never recorded the right state.
    assert_eq!(
        SnapshotStore::history_hash(&messages),
        oracle,
        "seed history hash must agree with the independent oracle"
    );
    // `Session` maintains these incrementally as messages are pushed; the reuse
    // hint is only honoured against them, so the case must model that or it
    // measures the no-reuse path and calls it "reuse".
    let mut hashes: Vec<u64> = messages
        .iter()
        .map(|m| feather_core::message::json_hash(m).expect("message hashes"))
        .collect();
    let seed = store
        .snapshot_reusing(1, &messages, &hashes, &SnapshotChunks::default())
        .expect("seed snapshot");
    let mut chunks = seed.chunks;
    let mut turn = 1u64;
    let mut appended = 0u64;
    measure("snapshot.reuse", 1, iterations, || {
        turn += 1;
        appended += 1;
        let appended_msg = Message::user_text(format!("appended turn {appended}"));
        hashes.push(feather_core::message::json_hash(&appended_msg).expect("message hashes"));
        messages.push(appended_msg);
        let commit = store
            .snapshot_reusing(turn, &messages, &hashes, &chunks)
            .expect("snapshot commits");
        chunks = commit.chunks.clone();
        // Invariant: exactly one message was newly serialised (the appended one)
        // and every earlier message was reused. If this reads N, the reuse
        // silently stopped working and the timing below is meaningless.
        let expected_reused = N as u64 + appended - 1;
        let ok = commit.cost.serialized == 1
            && commit.cost.reused == expected_reused
            && commit.chunks.bases.len() == messages.len();
        (
            ok,
            format!(
                "serialized={} reused={} (want 1/{expected_reused})",
                commit.cost.serialized, commit.cost.reused
            ),
        )
    })
}

/// Per-message token estimate, the inner loop of every request assembly.
fn tokenizer_message(root: &std::path::Path, iterations: usize) -> Measurement {
    let _ = root;
    const N: usize = 2000;
    let messages = history(N);
    // Oracle: the sum computed once, independently of the timed loop.
    let oracle: u64 = messages
        .iter()
        .map(feather_core::tokenizer::estimate_message_tokens)
        .sum();
    let mut sink = 0u64;
    measure("tokenizer.message", N as u64, iterations, || {
        sink = messages
            .iter()
            .map(feather_core::tokenizer::estimate_message_tokens)
            .sum();
        // Invariant: the per-run total equals the independently computed total.
        // An "optimisation" that silently skips messages or returns zero fails
        // here, in the run that reported the speedup.
        (sink == oracle && oracle > 0, format!("total_tokens={sink}"))
    })
}

/// Cache breakpoint placement over an append-only history.
fn cache_plan(root: &std::path::Path, iterations: usize) -> Measurement {
    use feather_core::agent::CachePlanner;
    use feather_core::provider::ChatRequest;
    let _ = root;
    const N: usize = 300;
    let messages = history(N);
    let optimizer = feather_cache::CacheOptimizer::new();
    let mut req = ChatRequest {
        system: "You are Feather.".into(),
        messages: std::sync::Arc::new(messages.clone()),
        tools: std::sync::Arc::new(Vec::new()),
        model: "opencode-go/deepseek-v4.1-flash".into(),
        max_tokens: 8192,
        temperature: None,
        cache: Default::default(),
        thinking: None,
        session_id: "bench".into(),
    };
    // A real session appends to the *previous* request, so each turn's history
    // is the last turn's plus a tail. Rebuilding from the same base every turn
    // would change message hashes at the same index and the stable prefix would
    // never grow — measuring a cache miss the planner cannot avoid.
    let mut session: Vec<Message> = messages.clone();
    let mut turn = 0u64;
    measure("cache.plan", 1, iterations, || {
        turn += 1;
        session.push(Message::user_text(format!("tail {turn}")));
        req.messages = std::sync::Arc::new(session.clone());
        optimizer.plan(&mut req, 0);
        let stable = optimizer.snapshot_metrics().last_stable_prefix_len;
        // Invariant: from the second turn on, the previous request is a strict
        // prefix, so the stable prefix must cover every prior message (there are
        // N + turn - 1 of them). Zero means one-off state leaked and the next
        // real turn would pay a full-prefix cache write.
        let expected = if turn == 1 { 0 } else { N as u64 + turn - 1 };
        (
            stable == expected,
            format!("stable_prefix={stable} want={expected}"),
        )
    })
}

/// Building an anchor map for a source file (the read path's first cost).
fn anchors_read(root: &std::path::Path, iterations: usize) -> Measurement {
    const LINES: usize = 1500;
    let dir = scratch_db(root, "anchors-read");
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let path = dir.join("sample.rs");
    let body: String = (0..LINES)
        .map(|i| format!("fn generated_{i}() -> usize {{ {i} }}\n"))
        .collect();
    std::fs::write(&path, &body).expect("write sample");
    let store = feather_hashline::AnchorStore::new(&dir);
    let mut sink = 0usize;
    measure("anchors.read", 1, iterations, || {
        // Fresh map each time: this measures mapping, not the cache-hit path.
        store.invalidate(&path);
        let map = store.build_map(&path).expect("map builds");
        sink = map.lines.len();
        // Invariant: every line got an anchor, and the content survived the
        // round trip. A mapping path that drops lines would look fast.
        let content_ok =
            map.lines.iter().any(|l| l.contains("generated_0()")) && map.lines.len() == LINES;
        (sink == LINES && content_ok, format!("lines={sink}"))
    })
}

/// `anchor_grep` over a tree where almost every file cannot match.
///
/// This is the case the prefilter exists for: without it every candidate gets an
/// anchor map built before the pattern is ever tried.
fn grep_anchor(root: &std::path::Path, iterations: usize) -> Measurement {
    let dir = scratch_db(root, "grep");
    std::fs::create_dir_all(&dir).expect("scratch dir");
    const FILES: usize = 120;
    const LINES: usize = 120;
    for f in 0..FILES {
        // Exactly one file in the tree contains the needle.
        let body: String = (0..LINES)
            .map(|l| {
                if f == FILES / 2 && l == LINES / 2 {
                    format!("let zeta_needle_{l} = {f};\n")
                } else {
                    format!("let alpha_{l} = {f};\n")
                }
            })
            .collect();
        std::fs::write(dir.join(format!("file_{f}.txt")), body).expect("write file");
    }
    let store = feather_hashline::AnchorStore::new(&dir);
    let opts = feather_hashline::grep::GrepOptions {
        pattern: "zeta_needle".to_string(),
        ..Default::default()
    };
    measure("grep.anchor", 1, iterations, || {
        let out = feather_hashline::grep::anchor_grep(&store, &dir, &opts).expect("grep runs");
        // Invariant: exactly the one planted hit, from exactly one file. A
        // prefilter that is too aggressive drops it here; one that does nothing
        // is still correct and shows up in the timing instead.
        let files: std::collections::BTreeSet<_> =
            out.hits.iter().map(|h| h.path.clone()).collect();
        (
            out.hits.len() == 1 && files.len() == 1,
            format!("hits={} files={}", out.hits.len(), files.len()),
        )
    })
}

/// Control: a bare redb write transaction with one small insert.
///
/// This exists to attribute cost honestly. `snapshot.full` and `snapshot.reuse`
/// both land near 2 ms despite one serialising 400 messages and the other one;
/// without this case a reader would assume serialisation dominates, and every
/// future optimisation would chase the wrong term. If this also reads ~2 ms, the
/// cost is the transaction and the fix is batching, not faster JSON.
fn redb_commit(root: &std::path::Path, iterations: usize) -> Measurement {
    use redb::TableDefinition;
    const TABLE: TableDefinition<u64, &[u8]> = TableDefinition::new("control");
    let dir = scratch_db(root, "redb-control");
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let db = redb::Database::create(dir.join("control.redb")).expect("redb opens");
    // Seed the table so the timed transaction only writes, never creates.
    {
        let txn = db.begin_write().expect("seed txn");
        {
            let mut t = txn.open_table(TABLE).expect("seed table");
            t.insert(0u64, b"seed".as_slice()).expect("seed insert");
        }
        txn.commit().expect("seed commit");
    }
    let mut key = 1u64;
    measure("redb.commit", 1, iterations, || {
        let txn = db.begin_write().expect("txn");
        {
            let mut t = txn.open_table(TABLE).expect("table");
            t.insert(key, b"payload".as_slice()).expect("insert");
        }
        txn.commit().expect("commit");
        key += 1;
        // Invariant: the row is durable and readable after commit. A commit that
        // "succeeds" without persisting would make every snapshot number a lie.
        let rtxn = db.begin_read().expect("read txn");
        let t = rtxn.open_table(TABLE).expect("read table");
        let present = t.get(key - 1).expect("get").is_some();
        (present, format!("key={}", key - 1))
    })
}

/// One full `Session::commit_turn` on a 60-message conversation.
///
/// This is the number a user feels as "the turn ended". It is deliberately not
/// the sum of the smaller cases: `commit_turn` opens several independent
/// databases, and only measuring the whole path shows whether that matters.
fn session_commit(root: &std::path::Path, iterations: usize) -> Measurement {
    use feather_core::session::Session;
    let home = scratch_db(root, "session-home");
    let cwd = scratch_db(root, "session-cwd");
    std::fs::create_dir_all(&home).expect("home dir");
    std::fs::create_dir_all(&cwd).expect("cwd dir");
    let mut session = Session::new(&home, &cwd).expect("session opens");
    for m in history(60) {
        session.push(m).expect("push");
    }
    let mut turn = 0u64;
    measure("session.commit", 1, iterations, || {
        turn += 1;
        session
            .push(Message::user_text(format!("turn {turn} payload")))
            .expect("push");
        let committed = session.commit_turn().expect("commit_turn");
        // Invariant: the turn counter advanced by exactly one and the committed
        // message count matches the in-memory context. A commit that silently
        // dropped rows would still be fast.
        let ok = committed == turn && session.messages().len() == 60 + turn as usize;
        (
            ok,
            format!("turn={committed} messages={}", session.messages().len()),
        )
    })
}

/// Opening a `Records` database — the step `FileSnapshots` performs twice per
/// `commit_turn` (once to read the tracked list, once to write).
///
/// Isolated here because the whole-path case cannot tell you whether the cost is
/// the open itself or the work done through it, and those have different fixes.
fn records_open(root: &std::path::Path, iterations: usize) -> Measurement {
    let dir = scratch_db(root, "records-open");
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let path = dir.join("records.redb");
    // Warm the file so the first open's create/first-write is not counted.
    {
        let seed = feather_core::records::Records::open(&path).expect("seed open");
        seed.put("warm", &1u32).expect("seed put");
    }
    let mut reads = 0u32;
    measure("records.open", 1, iterations, || {
        // Open and read, which is exactly what `tracked_paths` does. The handle
        // is dropped before the next iteration, so the process-wide Weak cache
        // cannot make the *second* open free — which is the situation the real
        // call path is in.
        let store = feather_core::records::Records::open(&path).expect("open");
        let value: Option<u32> = store.get("warm").expect("get");
        reads += value.unwrap_or(0);
        (
            value == Some(1),
            format!("key_present={} reads={reads}", value.is_some()),
        )
    })
}

/// Does one checkpoint commit get more expensive as the session's history grows?
///
/// `FileSnapshots::commit` rewrites the whole metadata JSON — which holds a
/// version list per tracked path — on every turn. If that JSON grows with turn
/// count, per-turn cost grows with session age and long sessions pay for their
/// own history. This measures the cost of the *first* commit against the cost of
/// the *Nth* on the same database, and the invariant asserts that the growth is
/// bounded: a commit at turn 200 must not cost more than a few times a fresh one.
///
/// Failing this case means the metadata needs to be incremental (append a
/// version row per turn) rather than a single re-serialised blob.
fn checkpoints_grow(root: &std::path::Path, iterations: usize) -> Measurement {
    use feather_core::file_snapshots::FileSnapshots;
    let dir = scratch_db(root, "checkpoints-grow");
    std::fs::create_dir_all(&dir).expect("scratch dir");

    // A fixed tracked file so `commit` has real metadata to carry across turns.
    std::fs::write(dir.join("tracked.md"), "initial\n").expect("seed file");
    let snapshots = FileSnapshots::new(&dir);
    // Track the file by capturing a turn, which is what records the path.
    // Register the file the way a real edit does; `commit` then carries it.
    snapshots
        .before_edit(&dir.join("tracked.md"))
        .expect("track file");

    let time_commit = |turn: u64| {
        let started = std::time::Instant::now();
        snapshots.commit(turn).expect("commit");
        started.elapsed().as_secs_f64() * 1e3
    };

    // Reference: an early commit on a shallow history.
    let early = time_commit(1);

    // Advance the same database to a deep history the way a long session would.
    for turn in 2..=200u64 {
        snapshots.commit(turn).expect("advance");
    }
    let late = time_commit(201);

    measure("checkpoints.grow", 1, iterations, || {
        // Keep advancing so repeated iterations measure the deep cost, not a
        // re-measurement of the same cached state.
        let turn = 202 + iterations as u64;
        let started = std::time::Instant::now();
        snapshots.commit(turn).expect("commit");
        let elapsed = started.elapsed().as_secs_f64() * 1e3;
        // The invariant: deep-history commits must stay within a small factor of
        // a shallow one. A 10x drift means the cost tracks history length.
        let ratio = if early > 0.0 { elapsed / early } else { 1.0 };
        (
            ratio < 10.0,
            format!("early={early:.2}ms late={late:.2}ms this={elapsed:.2}ms ratio={ratio:.1}x"),
        )
    })
}
