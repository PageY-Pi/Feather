# Feather 1.1.4 Windows installer

Share `dist/Feather-1.1.4-Setup-x64.exe` (Windows x64). Unsigned: Windows may
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
anyone's private configured models.

## Host prerequisites (Git Bash and git)

Feather runs model-issued commands through **Git Bash** and records file
checkpoints with **git**. Neither ships inside the installer: bundling them adds
roughly 60 MB and brings a GPLv2 redistribution obligation into a package that
currently carries only Feather's own binaries.

Setup therefore detects Git for Windows and, when it is missing, shows a
**Host prerequisites** page with three choices:

1. **Download and install Git for Windows now** (default) — fetches the current
   64-bit installer, runs it silently, then re-checks. Needs network access;
   Windows may ask for elevation because Git's own installer is per-machine.
2. **Open a mirror** in the browser for a manual install.
3. **Skip** — Feather installs anyway. The Bash tool and checkpoints stay
   unavailable, and both the installer and the app say so instead of failing
   later with an opaque tool error.

### Download source: mainland-China mirrors first

GitHub's release host is routinely blocked or throttled in mainland China, which
would turn this step into a long timeout. Setup therefore resolves the download in
this order, each attempt independent:

1. **USTC** (中国科学技术大学开源镜像站) —
   `https://mirrors.ustc.edu.cn/github-release/git-for-windows/git/LatestRelease/`
   It keeps a stable `LatestRelease` directory naming the current assets, so one
   small index fetch yields the exact file.
2. **npmmirror** (阿里巴巴镜像) — `registry.npmmirror.com/-/binary/git-for-windows/`
   One directory per release; setup reads the listing, picks the newest **stable**
   version (pre-releases are ignored) and then the installer inside it.
3. **GitHub releases API** — for machines outside China where the mirrors may be
   the unreachable side.

Both mirrors publish the official Git for Windows assets over HTTPS. The chosen
host is logged and named in the completion message so the source is never
implicit. Assets are selected by filename anchored on the name boundary, so
`MinGit-…` (a stripped archive) and `PortableGit-…` can never be installed by
mistake.

This step can never fail the installation: no network, a declined UAC prompt or
a failed Git setup only produce a message carrying both mirror URLs. When Git is
already present the page is skipped entirely.

Detection reads the `SOFTWARE\GitForWindows` `InstallPath` value, then the common
install roots, then `PATH` — where a `bash.exe` entry only counts when MSYS2 sits
beside it, because Windows also ships a WSL launcher under that name. The rules
live in `installer/prereq_git.iss` and are regression-tested by
`installer/prereq_probe.iss` (see Build below).

Build:
1. `cargo test --workspace`
2. `cargo build --release -p feather -p feather-gui`
3. Install Inno Setup 6, then `ISCC installer\Feather.iss`.
4. After changing prerequisite detection: `ISCC installer\prereq_probe.iss`,
   run `target\prereq-probe\feather-prereq-probe.exe /VERYSILENT`, and confirm
   every line in `%TEMP%\feather-prereq-probe.txt` reads PASS.

This version:
- 1.1.4 fixes why 1.1.3 still compacted on every turn. 1.1.3 changed the kept-span
  arithmetic, but the cut point itself was wrong in three ways. (1) The budget cut
  was clamped back to the newest plain answer, so a session whose only answer sat
  early — every later prompt being a long tool loop — kept its entire history:
  measured 584721 of 584746 tokens kept, one message summarized, and the rewrite
  stayed over the trigger. (2) The forward snap looked only for user messages, so
  mid-turn it jumped to the prompt appended at the tail and discarded the whole
  keep budget, collapsing the kept span to a single row. (3) The rewrite targeted
  `context − reserve` itself, the trigger line, so it landed a few tokens under it
  (live session: 256545 → 255587 against a 255616 trigger). Now the backwards move
  is charged against the keep budget, the budget cut snaps to the nearest safe
  boundary, and `compaction_room` reserves the keep budget below the trigger. The
  same 272k/16384/20000 session now rewrites 438583 → 20147 tokens with 235460 of
  headroom and compacts once.
- 1.1.3 stops compaction from firing on every turn after a long tool loop. The
  cut was strictly turn-granular: it never split a user span, and the clamp that
  protects the newest completed answer then dragged that whole span into the kept
  context. One prompt that runs hundreds of tool calls makes a single span most
  of the context, so a rewrite landed exactly on the trigger — the live session
  went 258177 → 255600 tokens against a 255616 trigger, 16 tokens of headroom —
  and the next turn compacted again, paying a summarization request each turn for
  no gain. `plan_cut_budget` now cuts inside an oversized turn at a boundary that
  keeps call/result pairs whole, keeps the newest answer's text, and the rewrite
  is measured against the room left before the trigger, so it always leaves real
  headroom instead of stopping on the line.
- 1.1.3 also stops `shrink_messages_to_tokens` from deleting the turn that holds
  the newest completed answer. When the summary plus the fresh prompt were the
  only user messages, the drain ran to the next user boundary and took the whole
  turn with it; oversized work is now elided in place instead.

- 1.1.2 makes model lists follow the provider. Feather compiled its catalog into
  the binary, so a provider model launched after a release stayed invisible until
  the next build. For the Volcengine Ark Coding Plan, Feather now reads the same
  `GET /api/coding/v3/models` endpoint a chat request is authorized against, in
  the background at startup, and caches the result in `config.redb` for 12 hours.
  That response is Ark's whole platform catalog (137 ids, 83 retired), so entries
  are filtered to callable chat models and folded onto the plan's documented names
  — newest build wins, retired and undocumented ids are dropped. Discovered models
  are unioned with the shipped ones, never substituted: offline, an auth failure or
  a changed payload removes nothing. `deepseek-v4.1-flash-economy` (same weights as
  the standard model, lower price, lower TPS) now appears without a rebuild.
- Line anchors are now five lowercase letters instead of four mixed-case
  letters. With both cases in the alphabet every anchor had 15 wrong-case
  spellings, and each was a valid anchor for a *different* line — a model that
  changed one letter's case edited the wrong line and no check could notice,
  because the line it hit really was unchanged. Lowercase makes that
  unrepresentable. The wider 26^5 space also covers every file Feather will map,
  where 52^4 fell short by about a million lines. Anchors saved by an older
  build are ignored and re-minted on the next read, so a 4-letter anchor left in
  an old conversation is refused with a re-read hint instead of being guessed at.
- 1.1.1 adds DeepSeek native web search. DeepSeek shipped the Search API on
  2026-10-17 (`POST https://api.deepseek.com/v1/tools/search`, $0.75 per 1,000
  successful calls, 5 QPS per account). Feather now routes `web_search` for the
  `deepseek` provider — and for any provider whose base URL is on deepseek.com —
  to that endpoint instead of scraping DuckDuckGo. One request returns ranked
  structured results, so it works with every DeepSeek model: the Responses-API
  `web_search` tool is v4-pro only and is ignored on `deepseek-flash`. Results
  render as index/title/URL/summary, keep at most eight entries, drop URLs the
  fetch guard refuses, and the query is cut to DeepSeek's 128-character limit.
  Bases configured for `/anthropic` or `/beta` are folded back to the host root.
  401/402/403/429 return a short sanitized reason and then fall back to
  DuckDuckGo like every other native backend. `web_fetch` is unchanged: it stays
  local (SSRF-guarded, no per-call fee) rather than proxying through
  `/v1/tools/fetch`.
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
- 1.1.0 adds a network rate limit next to the proxy indicator. Feather's model
  traffic (context upload plus streamed reply) is capped to a byte-per-second
  ceiling the user sets — typed as `512 KB/s`, `2 MB` or `Off`, or picked from
  presets. One reservation shaper covers all provider streams, so parallel
  subagents share the limit instead of each getting the full rate; a new limit
  applies from the next request. The CLI, RPC bridge and one-shot runs honour the
  same persisted value as the GUI. Control requests (OAuth sign-in) are never
  shaped, so authenticating cannot hang.
- 1.1.0 maps the system fonts read-only instead of copying them onto the heap.
  The CJK face alone was ~19 MB of private memory pinned for the whole session;
  mapped pages are file-backed, faulted in on use and evictable, so the GUI now
  settles around 52 MB with CPU rendering instead of 71 MB, and around 125 MB
  instead of 144 MB with GPU rendering. A typical Chinese transcript drops from
  ~159 MB to ~128 MB. Rendering is unchanged.
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
