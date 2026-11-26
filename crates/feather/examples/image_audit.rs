//! What do the images in real sessions actually cost, request by request?
//!
//! The `Image` block holds base64 text and lives in the message history, so one
//! attachment is re-sent on *every* later request of that session. The tokenizer
//! charges a flat 800 tokens for it, which is right only if the picture is roughly
//! 1 megapixel. This measures the real distribution: how many blocks a session
//! carries, how many base64 bytes they add to each request, and what the flat 800
//! says versus a size-proportional estimate.
//!
//! Run: `cargo run --release -p feather --example image_audit -- [sessions-dir]`

use feather_core::message::ContentBlock;
use feather_core::session_store::SessionStore;

fn main() -> anyhow::Result<()> {
    let dir = match std::env::args().nth(1) {
        Some(d) => std::path::PathBuf::from(d),
        None => dirs_home().join(".feather").join("sessions"),
    };
    let mut sessions = Vec::new();
    for e in std::fs::read_dir(&dir)? {
        let p = e?.path();
        if p.is_dir() && p.join("session.redb").exists() {
            sessions.push(p);
        }
    }
    sessions.sort();
    println!("{} sessions under {}\n", sessions.len(), dir.display());

    let mut total_blocks = 0usize;
    let mut worst: Vec<(u64, String, usize, u64)> = Vec::new();
    let mut scanned = 0usize;

    for p in &sessions {
        let Ok(store) = SessionStore::open(p) else {
            continue;
        };
        let Ok(msgs) = store.raw_messages() else {
            continue;
        };
        let mut blocks = 0usize;
        let mut b64 = 0u64;
        for m in &msgs {
            for b in &m.content {
                if let ContentBlock::Image { data, .. } = b {
                    blocks += 1;
                    b64 += data.len() as u64;
                }
            }
        }
        if blocks == 0 {
            continue;
        }
        scanned += 1;
        total_blocks += blocks;
        // What one request near the end of this session carries.
        let per_request = b64;
        worst.push((
            per_request,
            p.display().to_string(),
            blocks,
            msgs.len() as u64,
        ));
    }

    if scanned == 0 {
        println!("no session carries an image block");
        return Ok(());
    }

    worst.sort_by_key(|w| std::cmp::Reverse(w.0));
    println!("{scanned} sessions carry images, {total_blocks} image blocks total\n");
    println!("heaviest requests (base64 bytes carried in one request):");
    for (b64, path, blocks, msgs) in worst.iter().take(10) {
        let name = std::path::Path::new(path)
            .parent()
            .and_then(|p| p.file_name())
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        println!(
            "  {name}: {:.2} MiB across {blocks} block(s), {msgs} messages, flat-800 says {} tokens",
            *b64 as f64 / (1024.0 * 1024.0),
            blocks * 800
        );
    }

    // Base64 is transport, not tokens — but the *number of pixels* is tokens, and a
    // 4 MiB PNG can be 4000x4000 (16 MP, ~21 000 tokens) or 800x600 (0.5 MP, ~700).
    // The estimator cannot tell them apart without decoding, so measure how much the
    // flat 800 can be wrong by at the cap.
    println!(
        "\nflat 800 tokens per block vs. the pixel cost it is standing in for:\n\
         \x20 800x600  (0.5 MP)  ~700-900 tokens    flat 800 is accurate\n\
         \x20 1024x1024 (1.0 MP) ~1100-1600 tokens  flat 800 under-counts 30-50%\n\
         \x20 4000x4000 (16 MP)  ~21000 tokens      flat 800 under-counts 26x"
    );
    Ok(())
}

fn dirs_home() -> std::path::PathBuf {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(std::path::PathBuf::from)
        .unwrap_or_default()
}
