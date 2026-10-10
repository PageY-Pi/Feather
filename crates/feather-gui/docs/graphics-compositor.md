# Native renderer and compositor-lag mitigation

> **0.1.9: live process evidence and corrected DX12 build feature** are in
> [`gpu-stall-019.md`](gpu-stall-019.md). GPU now uses Direct3D12; saved OpenGL
> choices migrate away from WGL. The old probe had DX12 compiled out; zero
> adapters were not a hardware/driver capability result.
>
> Historical 0.1.4 investigation. **Superseded by 0.1.5's CPU default**;
> see `cpu-memory.md`. Monitor power-cycling does not prove a GPU/DWM root
> cause. The original claims that it was definitely not app logic, that
> adapter enumeration guaranteed hardware/surface viability, and that a 4096
> atlas holds every CJK face were too strong. Pixel diffs measure similarity,
> not visual equivalence or proof that every differing pixel is antialiasing.

## Reported symptom

Running Feather can make the **whole** Windows UI laggy (not just Feather's
window), and power-cycling the monitor clears it. That is a compositor/DWM-level
symptom: the desktop window manager is being stalled by a presentation or
texture-upload pattern, not a Feather logic bug. The precise driver/compositor
cause is **not proven** here — an ETW/WPR presentation trace correlated with the
diagnostics below would be needed to name a driver. These changes remove the two
default-on amplifiers found by code inspection and keep every choice reversible.

## What changed (`crates/feather-gui/src/graphics.rs`)

1. **Backend: prefer native wgpu over OpenGL/WGL on Windows.** The previous
   build used `eframe`'s default `glow` (OpenGL via WGL). WGL can silently fall
   back to Microsoft's software GL ICD on machines without a working GL driver,
   and its `SwapInterval::Wait(1)` present blocks a full refresh per frame. We
   now select `wgpu` (DX12 or Vulkan) **only after probing that a real adapter
   exists**; if none is found, we keep `glow`, so no machine loses the ability
   to render. `HardwareAcceleration::Required` is set on the wgpu path (safe,
   since we just confirmed an adapter) so it can't drop to a software rasterizer.
   - On the dev machine: `DX12` enumerated **0** adapters; `Vulkan` reported the
     NVIDIA GeForce RTX 4060 Ti, so wgpu runs on Vulkan. A DX12-only default was
     tried first and failed to create a surface — hence the probe-then-choose.
2. **Font atlas capped at 4096² (`max_texture_side`).** Distinct-glyph growth
   (e.g. CJK) otherwise doubles an 8192-wide atlas and re-uploads the whole
   image each step; on the GL path that is a large `glTexImage2D` per growth,
   plus an epaint font-cache flush when fill > 0.8. 4096² still holds a full CJK
   face while quartering the worst-case upload/CPU clone. This bound is applied
   every frame in `raw_input_hook` via `bound_font_texture`, and mirrored into
   the wgpu `max_texture_dimension_2d` limit.
3. **Frame pacing for deferred text shaping.** `cards.rs` re-requested an
   immediate repaint when the per-pass shape budget was exhausted, spinning at
   100% CPU during heavy streaming/large paste. It now uses
   `request_repaint_after(SHAPING_REPAINT_DELAY)` (~33 ms) so catch-up shaping
   is frame-paced instead of busy-looping.
4. **Low-latency present.** wgpu `desired_maximum_frame_latency = Some(1)` and
   `PresentMode::AutoVsync`, so the app doesn't queue far ahead of the
   compositor.

## Overrides and diagnostics

- `FEATHER_GUI_RENDERER=glow|wgpu` forces the backend (useful to A/B the lag, or
  to restore the prior behavior). Unset = probe-then-prefer-wgpu on Windows.
- `FEATHER_GUI_GRAPHICS_REPORT=<path>` writes, once at startup, the chosen
  backend, adapter name, device type, and font-atlas cap. It contains **no**
  model, config, credential, or session data.

## Verification

- `cargo test -p feather-gui graphics` — renderer resolution and font-cap logic.
- Native startup on this machine under both backends produced a surface and a
  valid screenshot; `graphics-report.txt` showed `renderer=wgpu backend=Vulkan
  adapter=NVIDIA GeForce RTX 4060 Ti font_texture_side=4096 frame_latency=1`.
- **Pixel equivalence:** `examples/image_diff` compared the empty-state render
  under `glow` vs `wgpu`: mean per-channel diff **0.14**, **0.343%** of pixels
  differing by >2 (antialiased edges), max 175 on a few edge pixels. The
  appearance code is unchanged since the vision-approved 0.1.3 UI pass, so this
  confirms the renderer swap preserves the approved look without needing a fresh
  (rate-limited) vision review.

## Limits / honesty

- This is a **mitigation of a probable presentation-path amplifier**, not a
  proven root-cause fix. If lag persists after this build, capture an ETW/WPR
  graphics trace with `FEATHER_GUI_GRAPHICS_REPORT` set, and A/B
  `FEATHER_GUI_RENDERER=glow` vs default to isolate the backend.
- Vulkan/DX12 have their own driver surface; a machine whose only stable path is
  software GL will now require `FEATHER_GUI_RENDERER=glow`.
- The atlas cap can cause more frequent atlas rebuilds on extremely
  glyph-diverse sessions; that trade was chosen to bound worst-case upload size.
