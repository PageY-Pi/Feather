//! Is `FileSnapshots` reopen-per-turn paying redb startup every turn?
//!
//! `Session::commit_turn` constructs a fresh `FileSnapshots` every turn and calls
//! `tracked_paths()` then `commit()` on it. Each instance has its own `OnceLock`, so
//! if nothing else holds the handle, every turn pays a full redb open. This measures
//! the pieces separately so the fix targets the real cost.
//!
//! Run: `cargo run --release -p feather --example records_cost`

use feather_core::records::Records;
use std::time::Instant;

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = std::env::temp_dir().join(format!("feather-records-cost-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("checkpoints.redb");
    // Seed so the file is not empty: an absent-file `create` is a different path.
    {
        let seed = Records::open(&path)?;
        seed.put("state", &serde_json::json!({"turn": 1, "paths": {}}))?;
    }

    // Cold: no handle held between iterations, which is the per-turn situation.
    let mut cold = Vec::new();
    for _ in 0..5 {
        let t = Instant::now();
        {
            let store = Records::open(&path)?;
            let _: Option<serde_json::Value> = store.get("state")?;
        }
        cold.push(ms(t));
    }

    // Warm: one handle kept alive across opens, which the process-wide Weak cache
    // should already be doing when something holds it.
    let keeper = Records::open(&path)?;
    let mut warm = Vec::new();
    for _ in 0..5 {
        let t = Instant::now();
        {
            let store = Records::open(&path)?;
            let _: Option<serde_json::Value> = store.get("state")?;
        }
        warm.push(ms(t));
    }
    let _: Option<serde_json::Value> = keeper.get("state")?;

    // Read alone, no open: the pure redb begin_read + get cost.
    let mut read_only = Vec::new();
    for _ in 0..5 {
        let t = Instant::now();
        let _: Option<serde_json::Value> = keeper.get("state")?;
        read_only.push(ms(t));
    }

    let median = |v: &mut Vec<f64>| {
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        v[v.len() / 2]
    };
    println!(
        "cold open+read   {:>6.2} ms\n\
         warm open+read   {:>6.2} ms  (handle held elsewhere)\n\
         read only        {:>7.3} ms",
        median(&mut cold),
        median(&mut warm),
        median(&mut read_only)
    );
    drop(keeper);
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}
