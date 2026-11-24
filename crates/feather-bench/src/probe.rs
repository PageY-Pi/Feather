//! Ad-hoc attribution probe.
//!
//! Temporary: this exists to find out which sub-step dominates before changing
//! anything. It is not part of the ruler's registry, because it measures steps in
//! isolation and the whole point of the `session.commit` case is that isolated
//! steps do not add up to the whole.
//!
//! Run with `feather-bench --probe`.

pub fn probe(root: &std::path::Path) {
    use std::time::Instant;

    let dir = root.join("probe-session");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let home = dir.join("home");
    let cwd = dir.join("cwd");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&cwd).unwrap();

    let mut session = feather_core::session::Session::new(&home, &cwd).unwrap();
    for i in 0..60 {
        session
            .push(feather_core::message::Message::user_text(format!(
                "msg {i}"
            )))
            .unwrap();
    }
    // Warm so one-time opens are excluded.
    session
        .push(feather_core::message::Message::user_text("warm"))
        .unwrap();
    let warm = Instant::now();
    session.commit_turn().unwrap();
    println!("warm commit: {:.2} ms", warm.elapsed().as_secs_f64() * 1e3);

    let sdir = session.dir().to_path_buf();

    // (b2/d2) the same two steps on a *hot* instance. The per-round numbers
    // above build a fresh `FileSnapshots` each time, so they include whatever
    // the store open costs; these two answer "what does the step itself cost
    // once the handle is warm", which is the situation inside `commit_turn`
    // after the first turn.
    let hot = feather_core::file_snapshots::FileSnapshots::new(&sdir);
    let t = Instant::now();
    let hot_tracked = hot.tracked_paths().unwrap_or_default();
    println!(
        "hot tracked_paths: {:.2} ms ({} paths)",
        t.elapsed().as_secs_f64() * 1e3,
        hot_tracked.len()
    );
    let t = Instant::now();
    hot.commit(9000).unwrap();
    println!("hot commit: {:.2} ms", t.elapsed().as_secs_f64() * 1e3);
    for round in 0..3u64 {
        session
            .push(feather_core::message::Message::user_text(format!(
                "round {round}"
            )))
            .unwrap();

        // (a) the compaction scan over all messages
        let t = Instant::now();
        let (_, writes) = feather_core::compaction::file_ops(session.messages());
        let d_fileops = t.elapsed();

        // (b) reading the tracked-path list
        let t = Instant::now();
        let tracked = feather_core::file_snapshots::FileSnapshots::new(&sdir)
            .tracked_paths()
            .unwrap_or_default();
        let d_tracked = t.elapsed();

        // (c) the checkpoint precheck (pure fs)
        let t = Instant::now();
        let checkpoints = feather_core::gitckpt::GitCheckpoints::open(&cwd);
        let precheck = checkpoints.precheck(&sdir, &[]);
        let d_precheck = t.elapsed();

        // (d) the file-snapshot commit
        let t = Instant::now();
        feather_core::file_snapshots::FileSnapshots::new(&sdir)
            .commit(1000 + round)
            .unwrap();
        let d_fscommit = t.elapsed();

        // (e) the message snapshot
        let t = Instant::now();
        let hashes: Vec<u64> = session
            .messages()
            .iter()
            .map(|m| feather_core::message::json_hash(m).unwrap())
            .collect();
        let _ = session
            .snapshots()
            .unwrap()
            .snapshot_reusing(
                2000 + round,
                session.messages(),
                &hashes,
                &Default::default(),
            )
            .unwrap();
        let d_snap = t.elapsed();

        // (e2) the per-turn catalog write: open the catalog database, write one
        // row, drop the handle. Mirrors `Session::register_catalog`, which runs
        // every turn against the same path.
        let t = Instant::now();
        {
            const ROW: redb::TableDefinition<&str, &[u8]> = redb::TableDefinition::new("catalog");
            let catalog = home.join("sessions").join("catalog.redb");
            if let Ok(db) = feather_core::database::open(&catalog)
                .or_else(|_| feather_core::database::create(&catalog))
            {
                if let Ok(txn) = db.begin_write() {
                    {
                        if let Ok(mut table) = txn.open_table(ROW) {
                            let _ = table.insert("probe", b"{}".as_slice());
                        }
                    }
                    let _ = txn.commit();
                }
            }
        }
        let d_catalog = t.elapsed();

        // (f) the whole thing
        let t = Instant::now();
        session.commit_turn().unwrap();
        let d_whole = t.elapsed();

        let ms = |d: std::time::Duration| d.as_secs_f64() * 1e3;
        println!(
            "round {round}: file_ops={:.2} tracked={:.2} precheck={:.2} fs_commit={:.2} msg_snap={:.2} catalog={:.2} | whole={:.2} (writes={} tracked={} precheck={:?})",
            ms(d_fileops),
            ms(d_tracked),
            ms(d_precheck),
            ms(d_fscommit),
            ms(d_snap),
            ms(d_catalog),
            ms(d_whole),
            writes.len(),
            tracked.len(),
            precheck.map(|(a, _)| a.len()),
        );
    }
}
