//! What would it cost to cap `@image.png` mentions the way pastes are capped?
//!
//! The paste path now downscales to a 1568 long edge before encoding. The mention
//! path reads a file and sends it unchanged, so the *same screenshot* costs about 9x
//! more when mentioned than when pasted. Fixing that means decoding the file,
//! resampling and re-encoding, which is all extra work — this measures whether that
//! work is affordable before any of it is written.
//!
//! Run: `cargo run --release -p feather-gui --example mention_downscale_cost`

fn main() {
    println!("decode -> resize -> re-encode -> base64, at the sizes a mention can carry\n");
    println!(
        "{:>12}  {:>9}  {:>9}  {:>11}  {:>11}  {:>10}",
        "pixels", "PNG", "capped", "decode+encode", "total time", "bytes saved"
    );
    for (w, h) in [(1920u32, 1080u32), (3840, 2160), (6000, 3000), (8000, 8000)] {
        // A screenshot-like image: mostly flat with content-like noise on part of it.
        let mut rgba = Vec::with_capacity(w as usize * h as usize * 4);
        let mut seed = 0x1234_5678_9abc_def0u64;
        for y in 0..h {
            for x in 0..w {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                // Left half looks like a document (flat), right half like content.
                if x < w / 2 {
                    rgba.extend_from_slice(&[248, 248, 250, 255]);
                } else {
                    rgba.extend_from_slice(&[
                        (x % 256) as u8,
                        (y % 256) as u8,
                        (seed & 0x1f) as u8,
                        255,
                    ]);
                }
            }
        }

        // The file a user would mention: PNG on disk.
        let png = encode_png(w, h, &rgba);
        let t = std::time::Instant::now();

        // Now the work a cap would add: decode it back, resample, re-encode.
        let decoded = image::load_from_memory(&png).expect("decode").to_rgba8();
        let scale = 1568.0 / w.max(h) as f64;
        let (cw, ch) = (
            ((w as f64 * scale).round() as u32).max(1),
            ((h as f64 * scale).round() as u32).max(1),
        );
        let small =
            image::imageops::resize(&decoded, cw, ch, image::imageops::FilterType::Triangle);
        let capped = encode_png(cw, ch, small.as_raw());
        let elapsed = t.elapsed();

        // What the wire carries, before and after base64.
        let before = png.len().div_ceil(3) * 4;
        let after = capped.len().div_ceil(3) * 4;
        println!(
            "{:>5}x{:<6}  {:>8.2}M  {:>8.2}M  {:>10.1?}  {:>10.1?}  {:>9.1}%",
            w,
            h,
            png.len() as f64 / (1024.0 * 1024.0),
            capped.len() as f64 / (1024.0 * 1024.0),
            elapsed,
            elapsed,
            100.0 * after as f64 / before as f64
        );
    }
    println!(
        "\nthe mention path runs once per send, off the render loop, so the question is\n\
         whether the added decode pays for itself in bytes carried on every later request"
    );
}

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
