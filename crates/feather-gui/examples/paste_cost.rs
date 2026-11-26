//! What does a pasted screenshot cost end to end, and how big does it get?
//!
//! A clipboard paste runs on the GUI thread that handled the keystroke and then
//! lives in the message history, so it is paid twice: once as a stall when pasted,
//! once as bytes on *every* later request. The paste path had no size cap, unlike
//! the mention path's 4 MiB, so this measures where the real ceiling is — and the
//! lines marked `->` show the 1568 long-edge cap that `MAX_LONG_EDGE` now applies.
//!
//! What it found, and why the cap exists: a 4K paste was 81 ms of GUI-thread work
//! and then **21.7 MiB re-sent with every later request**; 8K was 330 ms and
//! 86.7 MiB. Capping first makes 4K 65 ms and 2.5 MiB — faster *and* 8.7x smaller,
//! because PNG compressing 126 MiB of RGBA costs more than the resample does. Below
//! 1080p the cap costs ~20 ms and only buys bytes, which is still worth it because
//! those bytes recur per request.
//!
//! Run: `cargo run --release -p feather-gui --example paste_cost`

/// The same encoder `clipboard_image::encode_png` uses.
fn encode_png(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
    let buffer: image::RgbaImage =
        image::ImageBuffer::from_raw(width, height, rgba.to_vec()).expect("buffer");
    let mut png = Vec::new();
    use image::ImageEncoder;
    image::codecs::png::PngEncoder::new(&mut png)
        .write_image(
            buffer.as_raw(),
            width,
            height,
            image::ExtendedColorType::Rgba8,
        )
        .expect("png");
    png
}

/// Feather's hand-written base64, copied so the example measures the real one.
fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            T[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            T[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

fn main() {
    println!("pasted screenshot: decode + PNG encode + base64, all on the GUI thread\n");
    println!(
        "{:>14}  {:>10}  {:>10}  {:>9}  {:>9}  {:>10}",
        "size", "RGBA", "PNG", "encode", "base64", "in request"
    );
    for (w, h, label) in [
        (1280u32, 720u32, "720p"),
        (1920, 1080, "1080p"),
        (2560, 1440, "1440p"),
        (3840, 2160, "4K"),
        (7680, 4320, "8K"),
    ] {
        // A screenshot is not uniform, so a flat fill would let PNG win unfairly.
        // A cheap gradient plus noise stands in for real screen content.
        let mut rgba = Vec::with_capacity(w as usize * h as usize * 4);
        let mut seed = 0x2545F4914F6CDD1Du64;
        for y in 0..h {
            for x in 0..w {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                rgba.extend_from_slice(&[
                    (x % 256) as u8,
                    (y % 256) as u8,
                    (seed & 0x3f) as u8,
                    255,
                ]);
            }
        }
        let t = std::time::Instant::now();
        let png = encode_png(w, h, &rgba);
        let encode = t.elapsed();
        // What a long-edge cap would save. 1568 is Anthropic's recommended maximum
        // long edge; anything larger is downscaled server-side anyway, so the extra
        // bytes buy nothing but upload time and per-request replay.
        const CAP: u32 = 1568;
        let (cw, ch) = if w.max(h) > CAP {
            let scale = CAP as f64 / w.max(h) as f64;
            (
                ((w as f64 * scale).round() as u32).max(1),
                ((h as f64 * scale).round() as u32).max(1),
            )
        } else {
            (w, h)
        };
        let t = std::time::Instant::now();
        let capped = if (cw, ch) == (w, h) {
            None
        } else {
            let img: image::RgbaImage =
                image::ImageBuffer::from_raw(w, h, rgba.clone()).expect("buffer");
            let small =
                image::imageops::resize(&img, cw, ch, image::imageops::FilterType::Triangle);
            let capped_png = encode_png(cw, ch, small.as_raw());
            // Resize, encode and base64 are one gesture on the GUI thread, so the
            // time that matters is all three together.
            let capped_b64 = base64(&capped_png);
            Some((capped_png, t.elapsed(), capped_b64))
        };
        if let Some((capped_png, resize_time, capped_b64)) = capped {
            println!(
                "{label:>14}  -> {cw}x{ch}: PNG {:>5.1}M, resize+encode {resize_time:>8.1?}, \
                 in request {:>5.1} MiB ({:.0}% of uncapped)",
                capped_png.len() as f64 / (1024.0 * 1024.0),
                capped_b64.len() as f64 / (1024.0 * 1024.0),
                100.0 * capped_b64.len() as f64 / (base64(&png).len() as f64),
            );
        }
        let t = std::time::Instant::now();
        let b64 = base64(&png);
        let b64_time = t.elapsed();
        println!(
            "{label:>14}  {:>9.1}M  {:>9.1}M  {:>8.1?}  {:>8.1?}  {:>7.1} MiB",
            rgba.len() as f64 / (1024.0 * 1024.0),
            png.len() as f64 / (1024.0 * 1024.0),
            encode,
            b64_time,
            b64.len() as f64 / (1024.0 * 1024.0),
        );
        std::hint::black_box(&b64);
    }
    println!(
        "\nthe encoded PNG is what enters the message history, so the last column is\n\
         re-sent with every later request in the session"
    );
}
