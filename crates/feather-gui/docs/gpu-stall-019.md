# 0.1.9 — Windows GPU stall investigation

## Live evidence (no display reset/process termination)

During the reported whole-desktop sub-1 FPS incident, running Feather PID58832
loaded `opengl32.dll` + `nvoglv64.dll` (NVIDIA RTX4060Ti, driver596.36, Windows
build26200, 165Hz DisplayPort). GPU counters were near idle, but UI thread56148
consumed ~2.5 CPU seconds per3 seconds, mostly kernel time. This was not an
oversized GPU workload or a busy model/tool worker.

Repeated process-local PSS thread-register samples showed ~60% at
`ntdll!ZwQueryInformationThread`. Local unwinding repeatedly showed:

```text
ZwQueryInformationThread
KERNELBASE!GetExitCodeThread / SwitchToThread
nvoglv64.dll (thread-state polling/yielding)
opengl32!wglMakeCurrent
Feather / eframe UI render path
```

This localizes the observed app stall to WGL context binding in NVIDIA's ICD.
Registry source `eframe0.29.1/native/glow_integration.rs::change_gl_context`
explicitly skips its is-current fast path on Windows, unbinding and rebinding
the context for rendering. Do NOT blindly enable that fast path: upstream
issue4289 documents broken multi-viewport rendering with that optimization.
We instead remove the affected API path from the Windows build.

Important limit: WPR CPU/GPU/DesktopComposition recording failed with
`0xc5585011` (not elevated). Process-local stacks identify the app-side driver
spin, not the exact internal NVIDIA/DWM monitor-state defect or causality for
every other desktop window. Monitor reset alone is not proof. No driver,
monitor, VPN, global GPU or Windows setting was altered.

## Corrected feature/build and renderer policy

The former DX12 probe falsely reported zero adapters because DX12 was NOT
compiled: eframe/egui-wgpu use wgpu with defaults disabled, enabling `metal`,
`webgpu`, `wgsl`, not `dx12`. Asking for Backends::DX12 at runtime cannot restore
a backend compiled out. Explicit Windows `wgpu22.1` feature `dx12` now enables
it. Probe now sees hardware NVIDIA Dx12 adapters and the CPU adapter.

- Windows GPU = wgpu/native **Direct3D12**, hardware, opaque canvas, FIFO,
  bounded two-frame latency. Not WGL; Vulkan only by explicit env override.
- Removed eframe default `glow` from Windows feature graph. OpenGL support
  remains non-Windows only. Windows no longer loads `nvoglv64.dll`; a Windows
  system `opengl32.dll` may still load from other dependencies, but no WGL
  context is used.
- Saved `OpenGl` and `glow`/`opengl` env choices map to Wgpu in memory. CPU
  remains default. No unrelated model/effort/proxy config rewrite.
- If native GPU startup fails before creating FeatherApp/session/handoff,
  start the CPU host, NOT OpenGL. Runtime/session errors are not swallowed.
  CPU rescue launches a fresh process (winit only permits one EventLoop per
  process), retaining unconsumed session/draft restart env. Forced preview
  no-adapter startup passed: CPU child produced a screenshot and closed.
- Reactive maintenance input uses predicted_dt0 so egui does not subtract
  16.7ms from the scheduled33ms wake on a165Hz screen. Input-triggered paint
  remains responsive; idle does not acquire a polling animation loop.
- Native minimize test found a real debug panic: eframe can layout the transient
  199x34 minimized client. Clamp sidebar minimum height and skip tiny/minimized
  whole-app layout while still servicing close/restart/agent events.

## Verification

DX12 synthetic sustained preview, while the old GUI remained untouched:
20s, 624 frames; steady mean interval30.931ms, max33.983ms; 0.547CPU seconds
per4s. The initial old WGL sample was ~2.5CPU seconds per3s. Different workloads
and short durations: not a controlled long-term systemwide proof.
Release build recheck: 20s, 752 frames; steady mean26.687ms/max45.213ms,
0.297CPU seconds per4s; Dx12 report and module list contained native D3D12/DXGI,
not NVIDIA OpenGL ICD. Both native CPU/DX12 smoke/restart tests passed again.
Full workspace suite and check passed, including **144 GUI tests**.

Native maximize/restore, minimize/restore, input, close/headerX and
CPU->Direct3D12->CPU session/draft restart passed. Minimized close with a60s
blocking worker passed. Geometry checks cover100/125/150% scale and changing
stream/usage values. Tests cover legacy selection migration and transient
minimized dimensions. Reopen the old GUI to load the new renderer; installation
alone cannot replace code already executing in its address space.

## UI + DeepSeek vision

Session/model/effort/render/proxy now share one28-point fixed-height footer
baseline. Fixed service slots, ellipsized identities, hide usage counters at
compact widths rather than wrap controls. Add API gutter/divider, scroll-rail
clearance, explicit Send affordance and modal state reviewed iteratively.

Actual synthetic PNG image blocks reviewed using `deepseek/deepseek-flash`
(DeepSeek V4.1 Flash); no Codex request. Five iterations. Final screenshots
`target/ui-review/deepseek-pass5-{normal,compact,renderer}.png`: **8/10 PASS**.
Review noted light disabled Send contrast and minor subjective selection-state
polish. Preview/API review is explicit, no tools, no real user text/credentials.
Images/diagnostics are not bundled with the installer.
