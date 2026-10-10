//! Offline allocation probe: compare egui 0.29 owned fonts with Feather's shared
//! borrowed fonts. Requested live Rust heap bytes are NOT Windows private bytes
//! or working set. Each scenario is a separate process, without GPU/user data.
use eframe::egui;
use std::{
    alloc::{GlobalAlloc, Layout, System},
    sync::atomic::{AtomicUsize, Ordering},
};
#[path = "../src/font_store.rs"]
mod font_store;
struct Counter;
static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);
fn add(bytes: usize) {
    let live = LIVE.fetch_add(bytes, Ordering::Relaxed) + bytes;
    PEAK.fetch_max(live, Ordering::Relaxed);
}
unsafe impl GlobalAlloc for Counter {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let p = System.alloc(layout);
        if !p.is_null() {
            add(layout.size());
        }
        p
    }
    unsafe fn dealloc(&self, p: *mut u8, layout: Layout) {
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
        System.dealloc(p, layout);
    }
    unsafe fn realloc(&self, p: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        let q = System.realloc(p, layout, size);
        if !q.is_null() {
            if size >= layout.size() {
                add(size - layout.size());
            } else {
                LIVE.fetch_sub(layout.size() - size, Ordering::Relaxed);
            }
        }
        q
    }
}
#[global_allocator]
static ALLOC: Counter = Counter;
fn mib(bytes: usize) -> f64 {
    bytes as f64 / 1048576.0
}
fn main() {
    let mode = std::env::args().nth(1).unwrap_or_else(|| "shared".into());
    let before = LIVE.load(Ordering::Relaxed);
    let mut fonts = egui::FontDefinitions::default();
    let faces = font_store::faces();
    for (name, data, families) in [
        ("feather_ui", faces.ui, vec![egui::FontFamily::Proportional]),
        (
            "feather_mono",
            faces.mono,
            vec![egui::FontFamily::Monospace],
        ),
        (
            "feather_cjk",
            faces.cjk.map(|v| v.1),
            vec![egui::FontFamily::Proportional, egui::FontFamily::Monospace],
        ),
    ] {
        if let Some(data) = data {
            // In owned mode subtract the shared store's bytes from the report,
            // since the historical loader moved its Vec directly into FontData.
            fonts.font_data.insert(
                name.into(),
                if mode == "owned" {
                    egui::FontData::from_owned(data.to_vec())
                } else {
                    egui::FontData::from_static(data)
                },
            );
            for family in families {
                let list = fonts.families.entry(family).or_default();
                if name == "feather_cjk" {
                    list.push(name.into());
                } else {
                    list.insert(0, name.into());
                }
            }
        }
    }
    let ctx = egui::Context::default();
    ctx.set_fonts(fonts);
    for frame in 0..40 {
        let mut input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1160.0, 820.0),
            )),
            max_texture_side: Some(4096),
            time: Some(frame as f64 / 60.0),
            ..Default::default()
        };
        input
            .viewports
            .entry(egui::ViewportId::ROOT)
            .or_default()
            .native_pixels_per_point = Some(1.25);
        let _ = ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.label("The login 中文界面测试");
                ui.monospace("中文工具输出 abcdefghijk 0123456789");
            });
        });
    }
    let store_bytes = [faces.ui, faces.mono, faces.cjk.map(|v| v.1)]
        .into_iter()
        .flatten()
        .map(|b| b.len())
        .sum::<usize>();
    let subtract = if mode == "owned" { store_bytes } else { 0 };
    println!("mode={mode} live_requested_mib={:.2} peak_requested_mib={:.2} shared_font_bytes={store_bytes}",mib(LIVE.load(Ordering::Relaxed).saturating_sub(before+subtract)),mib(PEAK.load(Ordering::Relaxed).saturating_sub(before+subtract)));
}
