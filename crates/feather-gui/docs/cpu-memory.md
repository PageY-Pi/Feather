# Feather 0.1.5 — shared fonts and CPU rendering

## Implemented

- System font files are read once into `OnceLock`-owned buffers. egui receives `FontData::from_static` borrowed slices, so its Context/FontsImpl/ab_glyph clones share the same stable bytes. No repeated leaks or unsafe lifetime extension; missing system fonts still fall back to egui defaults. Segoe, Cascadia and one YaHei/CJK face are retained; Chinese glyph tests pass.
- Windows defaults to an app-local CPU rasterizer and `softbuffer` native presentation. This bypasses eframe's GPU startup and adapter probing. No NVIDIA GL/Vulkan user-mode driver is initialized by our CPU backend. `opengl32.dll` was still mapped (OS/window-system dependencies); a mapped DLL alone is not proof of an initialized GL context.
- The CPU painter uses egui's existing tessellated meshes for rounded surfaces, shadows, font atlas and images: premultiplied gamma-space blending, bilinear sampling, fractional-DPI clip rects, partial texture updates/freeing, half-open triangle spans, opaque bulk fill, and reusable frame buffers. Excess framebuffer capacity is released on large-to-small resizing.
- `egui-winit` supplies input, clipboard, cursor, IME, DPI and accessibility action integration. The custom header's viewport commands drive the same native window operations. Host sleeps while idle and frame-paces active repaint requests. CPU screenshot support goes through the normal egui Screenshot event.
- The API editor was rereading/parsing redb and the bundled model catalog every paint. It now refreshes the view at most every five seconds, invalidates after model mutation, and immediately switches cached homes. Save/Clear and validation errors remain visible.

## Measured memory (MiB, Windows release; synthetic preview)

| Configuration | Working set | Private bytes |
|---|---:|---:|
| Previous 0.1.4 OpenGL / owned fonts | ~145 | ~145 |
| Previous 0.1.4 Vulkan / owned fonts | ~203 | ~222 |
| Shared fonts / OpenGL | ~96 | ~99 |
| Shared fonts / Vulkan | ~160 | ~179 |
| **Shared fonts / CPU default (final)** | **64.6** | **47.3** |
| CPU / populated small chat preview | ~67 | ~50 |

The standalone allocation counter (corrected shrinking-realloc accounting) measured **60.79 → 20.67 MiB** requested live Rust heap for an equivalent egui font/layout workload. Requested heap is not working set or Windows private commit. The static font bytes total 21,035,456 on this machine. At one DPI, avoiding two deep font clones accounts for ~40 MiB of directly verified savings.

Earlier reporting incorrectly equated working set/private bytes with just app heap, attributed all backend delta to gpu-allocator, and presented guessed remainder buckets as measured. Driver heaps, runtime, mapped code, injected hooks and allocator retained pages can overlap these OS metrics. The actual backend delta is measured but its complete internal allocation attribution is not.

## CPU latency (before final minor appearance edits)

At 1160 × 820 logical points, 125% DPI, native software presentation included:
- Welcome: ~6.5 ms/frame mean (raster ~4.9 ms).
- Chat: ~7.8 ms/frame mean (raster ~5.9 ms).
- API modal: ~23.2 ms/frame mean (raster ~19.3 ms), including initial config/font work. Former per-frame disk refresh had inflated this to ~60 ms.
- Synthetic thinking history: 140,000 characters drawn from 7,000 distinct CJK glyphs, ~18.6 ms/frame mean; cold maximum ~88 ms. Only visible/overscan text is shaped; text is not truncated.

These are short native smoke workloads, not promises of 60 FPS on every computer. Large or high-DPI windows and translucent modal backdrops cost more CPU. The existing accelerated paths remain available for those workloads.

## Validation

- Full workspace tests/check pass; GUI **118 tests**, including shared borrowed font identity/lifetime, Chinese glyph rendering, painter diagonal alpha coverage, clipping/partial texture/free, bilinear samples/fractional DPI, framebuffer shrinking and CPU-default selection.
- Owned native preview smoke: maximize/restore, minimize/restore, WM_CLOSE and fresh header close-button click passed. Typed native input exercised a no-crash path; existing headless input tests assert content. Full manual IME/accessibility screen-reader and OS clipboard end-to-end testing remains to do.
- Final real CPU screenshots (normal, compact API dialog, close hover, welcome, maximized) were attached to an image-capable review model: **8/10 PASS**. `target/ui-review/cpu-final` contains the captures.
- Empty screenshot first comparison with OpenGL: mean channel delta ~0.067 and 0.099% of pixels differed by >2. Other full-UI captures differ more because timing, hover, directory listing and initial scroll geometry can vary. The vision pass is separate from this numerical comparison.

## Controls / reproduce

Windows default is CPU. Set `FEATHER_GUI_RENDERER=cpu|software` explicitly, or choose `glow` / `wgpu` for accelerated rendering, then restart Feather. No global display settings are changed. Linux/macOS retain the prior accelerated default; the CPU host is primarily validated on Windows and currently softbuffer is built without Linux window-system features.

```text
cargo build --release -p feather -p feather-gui
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/measure-memory.ps1 -Renderers auto -Panel empty
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/cpu-smoke.ps1
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/capture-ui.ps1 -OutputDir target/ui-review/cpu-final
cargo run -p feather-gui --example mem_breakdown -- shared
cargo run -p feather-gui --example mem_breakdown -- owned
```

`FEATHER_GUI_GRAPHICS_REPORT=<path>` writes renderer/adapter metadata only. CPU `FEATHER_GUI_FRAME_REPORT=<path>` reports frame/raster timings on exit. Preview is synthetic, uses a temp home and no agent or live authentication/configuration. Measurement scripts terminate only their own previews. Installer still includes only two release EXEs and notices, not screenshots/reports/config/auth/sessions.

## What CPU rendering can and cannot fix

Rust has software rasterizers and native presentation crates, not a magic smaller hardware driver. `softbuffer` plus a small egui mesh painter avoids vendor GPU context allocation **inside Feather**. Windows still uses DWM/GPU to compose the desktop; neither this change nor monitor power-cycling establishes the original lag's root cause. If lag recurs, gather a graphics/compositor trace rather than claiming it is solved.

CPU rendering trades some active CPU cost for lower memory and removal of the app's former GPU-driver path. Idle painting is event-driven; dynamic texture/framebuffer memory can still grow for a big monitor. GPU custom paint callbacks / extra native egui viewports are not implemented on the software backend (the current Feather UI uses neither); unsupported callbacks return an explicit error instead of silently vanishing.
