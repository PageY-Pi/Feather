# Feather 1.1.0 Windows installer

Share `dist/Feather-1.1.0-Setup-x64.exe` (Windows x64). Unsigned: Windows may
show SmartScreen; this build has not been code-signed.

Per-user installation (no admin):
- Installs CLI + GUI under `%LOCALAPPDATA%\Programs\Feather`.
- Adds this folder to the **user PATH**, preserving existing entries.
- Creates Start-menu and desktop shortcuts.
- Registers **Open Feather here** for folders, folder backgrounds and desktop.
  On Windows 11 it may appear under **Show more options**.
- Offers to launch Feather; each friend adds their own URL/key/models via **+ Add API**.
- Uninstall removes its PATH entry when the installer added it and removes menu
  entries. It preserves the user's `.feather` data/configuration.

The installer `[Files]` section is an explicit allowlist of **two release EXEs plus the third-party license notice**.
No config.redb, models.json, auth.json, credentials, sessions or user files are
included. Embedded public model/protocol defaults are application data, not
anyone's private configured models. Git and Bash are not bundled: live file
checkpointing/tool execution may require Git for Windows to be installed.

Build:
1. `cargo test --workspace`
2. `cargo build --release -p feather -p feather-gui`
3. Install Inno Setup 6, then `ISCC installer\Feather.iss`.

This version:
- 1.1.0 keeps compaction aligned with Pi (trigger at context minus reserve, max
  output not subtracted) but moves the retained-token budget out of per-model
  settings: context/reserve/preserved tokens are now one global setting, and the
  effective window is `min(global context, model context window)`.
- 1.1.0 makes bundled providers read-only: their models, wire API and endpoints
  come from the compiled catalog, so only the credential is user-owned. The
  duplicate `volcano` provider was folded into `volcengine-plan`, and every
  Volcano model routes to the Ark coding endpoint.
- 1.1.0 memoizes the resolved model catalog and the session-name lookup, so RPC
  transcript encoding no longer rebuilds the catalog per message.
- Ordinary Backspace remains unchanged when no IME-owned key is reported.
- 1.0.0 efficiency review: shared request ownership (no deep history/tool-schema
  clone), cached token/hash/prefix planning, linear SSE/FIFO, combined audit/meta
  commits, borrowed compaction serialization and GUI buffer/menu reuse.
  See ../docs/code-review-1.0.0.md for measured synthetic results and limitations.
- Root README now serves as the GitHub intro/install/workflow guide.
- Footer identity labels use natural measured widths and a common 7pt separator
  gap on both groups. No fixed-slot dead air; Render/Proxy remain right aligned.
- Composer Send button removed (Enter sends/queues, Shift+Enter inserts a line).
- `/skill`, `/skill ` and `/skill:query` list fresh skill names/descriptions inline;
  choose via mouse or arrows+Tab/Enter, then type the request. Body remains lazy.
- `/rewind` and `/rewind ` show committed turn/request labels even while the agent
  lock is held, using an independent shared snapshot read handle. Loading/empty
  states visible; busy rewind execution still requires stopping the response.
- Local OAuth/settings/models/keybindings/GUI/proxy configuration is redb-only.
  Known legacy JSON files are transactionally imported/archived inside redb;
  successful cleanup leaves no loose JSON dependency. API JSON remains wire data.
- Compaction excludes model thinking from summary input and retained context;
  raw original thinking remains in the transcript audit.
- Live transcript offloads durable older rows (256-row/2MiB high-water), reloads
  older history from redb; streaming and in-flight tool rows are preserved.
- Footer: session · model · effort · TPS left, Render · Proxy right, no tokens.
  `/rewind ` uses command-style turn/user-request suggestions; proxy DEL removes.
- Active-monitor maximum FPS=refreshHz-1; reactive idle remains asleep.
  Hiddify port12334 discovery +saved custom local ports. Interactive TUI retired;
  plain `feather` launches GUI, headless/RPC bridge retained for automation.
- Windows GPU now uses native Direct3D12, not the NVIDIA/WGL context-binding
  path sampled during the live desktop-stall incident. Explicit DX12 build
  feature fixes the prior missing backend; legacy OpenGL choices migrate safely.
  CPU remains the default and native-GPU startup failure falls back to CPU.
- Single aligned session/model/effort/renderer/proxy footer; no second row.
  Composer Send affordance, stronger rail/gutter and restrained header/modal
  polish passed actual DeepSeek image review (8/10, five iterations).
- Transient minimized surface geometry no longer triggers negative-height
  layout panic. Native CPU/DX12 restart and close tests cover both hosts.
- Up to three non-quota retries at +5, +30, +60 seconds from initial error; cancel interrupts
  backoff and failed partial output is reset without replaying executed tools.
- `/skill` chooser and lazy `/skill name request` / `/skill:name request` invocation
  now connected in the GUI, using pi-compatible metadata/frontmatter loading.
- Working status replaced with `Proxy: Off` / selected proxy. Startup loopback
  proxy detection, redb catalog, direct/HTTP/SOCKS selection without restart.
- Unsafe checkpoint paths are blocked/marked illegal, and checkpoint errors are
  recoverable context diagnostics rather than aborting the whole response.
- Reserved TPS/token slots stabilize compact GPU footer/composer layout. The
  intentional conversation/input divider is restored, with no extra GPU seam.
- Loaded older redb pages are evicted scrolling downward, reloadable on demand.
- Click `Render: CPU` / `Render: GPU` at the bottom right to open the renderer
  chooser, like the session/model dialogs. Choice is saved; changing it restarts
  Feather and restores session/draft. The visible footer dropdown is removed.
- User message text is left-aligned inside the right-side bubble. GPU clears to
  the same opaque canvas as CPU, eliminating the one-pixel composer seam.
- Original conversation is independent of compacted LLM context. Resume uses
  original redb audit pages, with earlier history fetched as you scroll up.
  Summaries stay private; only the compaction completion notice is shown.
- Close/X/Alt+F4 now cooperatively cancel the agent with bounded shutdown and do
  not wait indefinitely on Tokio blocking jobs. CPU close progresses while
  minimized; no global drivers/display settings are changed.
- Windows defaults to CPU rasterization + softbuffer native presentation, avoiding
  app-side GPU-driver initialization. Idle preview: ~64 MiB working set (previous
  Vulkan build ~203 MiB). Accelerated backends remain selectable with
  `FEATHER_GUI_RENDERER=wgpu|cpu`; Windows `glow` aliases migrate to wgpu.
  See `crates/feather-gui/docs/gpu-stall-019.md` for evidence and limits.
- System-font data is shared once per process instead of deep-copied by egui;
  allocation probe: ~60.8 → 20.7 MiB. API editor caches model data between frames.
- Compaction fix: summaries no longer stream into the live reply, the last
  completed answer survives auto-compaction, savings count real characters
  (not just tool-result bytes), and no-op compaction prints no notice.
- Polished vector window controls, equal hit targets, destructive close hover,
  inward tooltips, reserved long-path space, and left-to-right keyboard focus.
- Responsive API dialog with visible internal scrolling, sticky Save/Clear,
  collapsible advanced settings, and a clearer modal backdrop.
- Adaptive large-window conversation width, refined composer/footer/shortcut chips.
  Iterative PNG vision review: final 9/10 PASS. See
  `crates/feather-gui/docs/ui-appearance-audit.md`.
- Session chooser: highlight with ↑/↓ then **Del**, confirm before deletion.
  No per-row Delete button; Del is consumed before the search field handles it.
- Leading `/`: command menu with right-side explanations; ↑/↓ choose, Tab/Enter
  select. Commands execute locally, not as model prompts. `/help` lists all GUI
  commands. Argument commands fill the input for completion.
- Manual efforts: `off, low, medium (med), high, xhigh, max`; checkbox
  **Reasoning not supported** limits a model to off. No dynamic fetching.
- **Feather** brand and custom borderless header: minimize, maximize/restore,
  close; fullscreen is F11 (no duplicate button). Drag the header, double-click to maximize.
- + Add API window has its own top-right close control.
- Pointer-hover selection in sessions/models/OAuth/commands, rounded Windows 11 window.
- Compiled `web_search` / `web_fetch` from the local pi-native-search design;
  native providers plus DuckDuckGo fallback, public-page fetching. See
  `crates/feather/docs/web-and-hover.md` for limits and supported backends.

Validation: workspace tests, optimized builds, installer compilation and a
per-user silent install passed. PATH/Explorer registry values and installed
binary version were verified on this machine. Not tested on a separate clean VM.
