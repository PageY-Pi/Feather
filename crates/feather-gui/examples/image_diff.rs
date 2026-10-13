//! Offline equivalence check: the wgpu/Vulkan renderer must produce the same
//! pixels as the previously vision-approved glow build. Compares two PNGs and
//! reports mean/max per-channel difference and the fraction of differing pixels.
fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let a_path = args
        .next()
        .ok_or_else(|| anyhow::anyhow!("usage: image_diff A.png B.png"))?;
    let b_path = args
        .next()
        .ok_or_else(|| anyhow::anyhow!("usage: image_diff A.png B.png"))?;
    let a = image::open(&a_path)?.to_rgba8();
    let b = image::open(&b_path)?.to_rgba8();
    if a.dimensions() != b.dimensions() {
        anyhow::bail!(
            "size mismatch: {:?} vs {:?}",
            a.dimensions(),
            b.dimensions()
        );
    }
    let (mut sum, mut max, mut diff_px) = (0u64, 0u32, 0u64);
    let mut n = 0u64;
    for (pa, pb) in a.pixels().zip(b.pixels()) {
        let mut changed = false;
        for c in 0..3 {
            let d = (pa[c] as i32 - pb[c] as i32).unsigned_abs();
            sum += d as u64;
            max = max.max(d);
            if d > 2 {
                changed = true;
            }
        }
        if changed {
            diff_px += 1;
        }
        n += 1;
    }
    let total = n * 3;
    println!("{} vs {}", a_path, b_path);
    println!("size: {:?}", a.dimensions());
    println!("mean per-channel diff: {:.4}", sum as f64 / total as f64);
    println!("max per-channel diff: {max}");
    println!(
        "pixels differing >2: {diff_px} / {n} ({:.3}%)",
        diff_px as f64 * 100.0 / n as f64
    );
    Ok(())
}
