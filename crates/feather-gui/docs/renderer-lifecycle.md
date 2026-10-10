# Renderer choice / close / restart — 0.1.6

## User-facing behavior

- The bottom-left selector offers **CPU** (Windows default) and **OpenGL**.
- The final footer row's right side displays **Render: CPU** or **Render: GPU**
  beside Ready / Working / Needs attention. The session/model/effort/TPS details
  remain above that row, with compact wrapping rather than footer clipping.
- A changed choice is saved to the `gui` config document. It does not overwrite
  settings, API keys, models, or the manual reasoning policy.
- Changing renderer automatically closes/relaunches Feather. Workspace, exact
  selected session, unsent composer draft, thinking visibility and tool expansion
  are handed off. The child waits for the previous process to exit before opening
  the redb session, and handoff overrides ordinary auto-resume-latest startup.
- Selection is disabled during an active response, a busy agent operation or a
  pending sign-in. Finish/stop that work first; a renderer switch never silently
  interrupts a response or discards pending queued prompts.
- Environment overrides still support `FEATHER_GUI_RENDERER=cpu|glow|wgpu`.

## Close hang mitigation

The GUI previously owned a bare Tokio Runtime. Runtime::drop can wait
indefinitely for spawn_blocking work (e.g. blocking DNS/auth), including when
an agent owns a pending request. That is a verified shutdown risk, not proof of
the cause of every reported freeze.

Both the header X and native WM_CLOSE/Alt+F4 now enter a shared close lifecycle:

1. Signal agent cancellation; reject new submits.
2. Stop rendering normal panels/modal database reads. Show a light Closing /
   Restarting view and keep the event loop alive while task completion arrives.
3. Normal close grants a three-second cancellation grace period. An idle close
   does not wait that long. A restart requires the agent to be idle and settings
   and draft handoff to have committed; a preparation failure stays in the app.
4. UiRuntime uses `shutdown_background()` instead of an unbounded destructor
   join. CPU close polling also advances while minimized, without repaint.
5. Hide/destroy the native window and release surfaces before launching a child.

Completed/committed turns are durable already. Cooperative cancellation commits
its normal tool result/snapshot. If a task ignores cancellation beyond the grace
period, normal close may terminate it without preserving unfinished in-flight
output; no forced save of a half-complete session is attempted.

## Verification

- `cargo test --workspace` and `cargo check --workspace` passed; GUI **127 tests**.
- New deterministic tests: nonblocking Runtime drop while a blocking task waits,
  CPU default and renderer-only persistence, one-shot long Unicode draft/exact
  session handoff, bounded cancellation, footer positions, and durable real-agent
  cancellation while a synthetic tool waits. The last test reopened the session
  after app drop and confirmed the prompt, cancellation result and turn snapshot.
- Native owned-preview smoke passed on **CPU and OpenGL**: maximize/restore,
  minimize/restore, WM_CLOSE and a fresh header-X click, even with a 60-second
  blocking task. Typed input is a no-crash smoke; headless tests check contents.
- Native selector smoke passed **CPU → OpenGL → CPU**, verifying changed PID and
  actual-backend reports each time, then minimized close with the blocking task.
- Only owned synthetic preview processes were closed. The user's live GUI was
  not terminated; no monitor/driver/global display changes were made.
- Final captures: `target/ui-review/renderer-selector-016`. Fresh image-model
  review was blocked by provider 429 usage limit; don't treat the prior 0.1.5
  vision pass as a new 0.1.6 vision result. Geometry and native behavior are tested.

## Reproduce

```text
cargo test --workspace
cargo check --workspace
cargo build --release -p feather -p feather-gui
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/cpu-smoke.ps1 -Renderer cpu
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/cpu-smoke.ps1 -Renderer glow
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/renderer-restart-smoke.ps1
```

Smoke scripts find visible windows belonging only to the spawned PID, handle DPI
and real pointer state, and restore the original pointer. A native OS helper
window has a zero-sized client area; using Process.MainWindowHandle blindly had
made the initial OpenGL smoke fail without ever clicking Feather's controls.
Installer allowlist is unchanged: two EXEs + third-party notices, no home data.
