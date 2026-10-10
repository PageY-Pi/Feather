# UI appearance audit — Feather 0.1.3

## 0.1.9 follow-up — DeepSeek vision

- One footer row: session/model/effort + renderer/proxy on a single baseline;
  usage slots never force an extra line at compact widths.
- Equal native-control geometry, stronger header delimiter/gutter, readable
  scrollbar rails, bounded tree labels, left composer hint and Send affordance.
- Windows GPU screenshots now use native DX12, not OpenGL.
- Actual PNG review using `deepseek/deepseek-flash`, five iterations. Final
  `deepseek-pass5-{normal,compact,renderer}.png`: **8/10 PASS**. No Codex calls.
- See `gpu-stall-019.md` for evidence/limitations/native validation.

## 0.1.7 follow-up

- User bubbles remain right-anchored; multiline/wrapped text uses an explicit
  left-aligned inner layout. Shape/galley tests cover compact and wide widths.
- Removed the permanent renderer dropdown. The right-footer `Render: CPU/GPU`
  label opens an accessible keyboard/pointer chooser with the same restart flow.
- CPU and GPU now use the same opaque canvas clear color. Adjacent antialiased
  panel edges previously exposed eframe's dark default clear color as a thin GPU
  composer seam. The fixed boundary is verified in native pixel captures.
- Renderer dialog polished with equal 40-point option rows, larger vector close
  target, compact width, responsive placement, and consistent row boundaries.
- Removed the redundant tool-header hover tooltip; added titlebar/footer padding.
- Final attached native PNG review: `target/ui-review/{main-glow-pass6.png,
  popup-compact-pass6.png,popup-glow-pass6.png}` — **8/10 PASS**.
- Existing close/native renderer restart tests remain passing. Full workspace
  suite passes including 134 GUI tests. Earlier 0.1.3 history below is retained.

## Shipped appearance changes

- A fixed-height 60-point header reserves the top-right controls before measuring the workspace path. Long paths truncate in their own clipped region.
- Equal 36 × 34-point window-control targets, vector minimize/maximize/restore/close icons, consistent hover/pressed/focus states, destructive close feedback, and inward-positioned tooltips.
- A separate + Add API action with a clear divider, tooltip, and the same editor-opening behavior as `/add`. Keyboard focus follows the visual left-to-right order; window dragging is not a tab stop. Maximize/restore retains a stable focus ID.
- Broad drag/double-click region without intercepting window-control clicks.
- More legible scroll affordances with reserved rail width. Consistent error surfaces are included in contrast tests. Tool-status/chevron spacing, empty-state shortcut chips, wrapped/ellipsized footer identities, and a composer separator improve visual hierarchy.
- API dialog: restrained introduction, unstriped aligned forms, wider inputs, compact margins, clear backdrop, internal scrolling, sticky Save/Clear, visible validation errors, and collapsible advanced settings. Empty configured-model state is explicit.
- Large windows use an adaptive, bounded conversation/composer width instead of a narrow fixed column. Normal-window text width is unchanged.

## Iterative vision validation

Rendered native Windows screenshots were attached as actual PNG image blocks to an image-capable reviewer, not described to it in text. Ten UI iterations included normal and compact windows, welcome, API dialog, compact API dialog, close hover, command menu, and a genuinely maximized viewport. Preview data is synthetic; credentials and live conversations are never part of the images.

The reviewer identified real issues (inconsistent controls, weak dialog scroll affordances, cramped form composition, tooltip positioning, and overly narrow large-window layout). Expected clipping at a scroll viewport boundary was distinguished from inaccessible controls using geometry/interaction tests; conversation history was not truncated or forced to fit.

**Final iteration (`target/ui-review/pass10`): 9/10 — PASS.** The final review approved alignment/hit targets, responsive clipping, compact dialog action access, close hover/tooltip, maximized balance, and command menu. Only a non-blocking subjective note about scrollbar weight remained. This is model review of tested screenshots, not a claim of perfect visuals on every display/OS.

A first attempt to post native mouse messages did not reliably establish hover; it was discarded. Final hover capture goes through eframe's raw-input hook, exercising the normal egui interaction/rendering path. Maximize is applied after initial sizing and captured after settling. The real user's running GUI is never controlled or terminated.

## Reproduce

```text
cargo build --release -p feather -p feather-gui
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/capture-ui.ps1
cargo run -p feather --example ui_vision -- <image-capable-provider/model> target/ui-review/final/normal.png target/ui-review/final/compact-add.png target/ui-review/final/close-hover.png target/ui-review/final/maximized.png
cargo test --workspace
```

The screenshot script launches and closes only its own offline preview processes. Preview starts with default settings and empty auth/catalog, no agent/session, and a temporary home. Model/auth mutations and browser login are blocked in preview. The review example requires explicit invocation, sends only the supplied PNGs, uses no agent tools or persisted session, and can incur model charges. Review is never invoked automatically by the shipped GUI.

## Regression checks

- Seven new chrome tests: long-path reservation at 760–1920 points; native viewport commands and Add API click isolation; destructive close hover; keyboard focus order and Enter activation; visible dialog actions/errors at 760 × 520 minimum size; bounded Unicode/footer IDs; and preview mutation/login guards.
- GUI suite: **105 passed**. Full workspace suite: **397 passed**, no failures. Workspace check and whitespace check pass. Existing diagnostic-test warnings were not changed.
- Release executable/installer rebuilt. Screenshot/report helpers and images are not bundled: installer still contains only the two executables and third-party notices, never config/auth/session databases.

## Limits

Native window rounding depends on Windows/DWM and is normally absent while maximized/fullscreen. The screenshots exercised this machine's Windows rendering and DPI; Linux/macOS, alternate fonts, accessibility tooling, and other DPI levels need their own visual passes. Scroll areas intentionally show partial boundary rows. The short main window controls are desktop mouse/keyboard targets, not touch-optimized 44-point controls.
