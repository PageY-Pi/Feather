<p align="center">
  <img src="crates/feather-gui/assets/feather-logo.png" alt="Feather" width="120" />
</p>

<h1 align="center">Feather</h1>
<p align="center"><strong>A native Rust coding agent. Small interface, serious tools.</strong></p>
<p align="center">Desktop GUI · Verified file edits · Durable sessions · Multiple model providers</p>

Feather is a standalone coding assistant for your workspace. It combines a native
**egui** desktop interface with a Rust agent runtime: streamed answers, real tools,
parallel subtasks, recoverable history and anchor-verified file editing. No browser
shell, Electron, Node.js runtime or dynamically loaded plugin engine is required.

**Version 1.0.5** · Windows x64 is the release-tested platform. Rust sources contain
other-platform support, but those builds are not claimed to be release-validated.

## Why Feather?

- **Native desktop UI.** Workspace tree, virtualized conversation, foldable tool
  and reasoning cards, keyboard-first controls and inline slash-command menus.
- **Edits you can verify.** Read tools return four-letter line anchors; replace,
  insert, copy, move and undo target those anchors, not guessed line numbers.
- **Your choice of provider.** Anthropic Messages, OpenAI-compatible Chat
  Completions, OpenAI/Codex Responses and Pi Messages. Add custom endpoints and
  saved models from **Add API**, with API-key or supported OAuth sign-in.
- **Durable local state.** Settings, credentials and preferences live in redb;
  sessions keep a raw audit plus turn snapshots. No loose JSON configuration is
  needed after migration.
- **Long-running work without retaining the entire UI history.** Older finalized
  transcript rows page out to redb and reload on demand. Context compaction is
  separate from visible history, and excludes model reasoning from its input.
- **Useful agent tools.** Bash, anchor-aware file search/editing, tracked tasks,
  web research and configurable subagents. Skills load their instructions lazily.
- **Per-app proxy and renderer choice.** HTTP or SOCKS5h proxies apply only to
  Feather. Windows supports CPU rendering and Direct3D 12; rendering is reactive
  with a maximum frame rate of the active monitor's refresh rate minus one.

## Install on Windows

Use `Feather-1.0.5-Setup-x64.exe` from the project's published release assets, or
build it locally following the instructions below. The per-user installer needs
no administrator privileges. It installs the two executables, adds Feather to
your user PATH, and provides an **Open Feather here** Explorer menu.

The installer is unsigned; Windows may show a SmartScreen warning. Only run
binaries from a source you trust. User credentials, sessions and databases are
not bundled in the installer.

Launch Feather from the Start menu, Explorer, or a terminal in your project:

```bash
feather                        # opens the desktop GUI in this directory
feather-gui /path/to/project    # opens a specific workspace
```

### First run

1. Open **Add API** and sign in to a supported provider or save an API key/custom
   endpoint. Provider subscriptions and access rules still apply.
2. Click the **model** label in the footer, or press **Ctrl+L**, to choose a model.
3. Choose reasoning effort from the footer, when supported by that model.
4. Type a request and press **Enter**. A follow-up entered during a response is
   queued; **Esc** stops the running response.

Without credentials Feather can run a deterministic offline provider for local
smoke tests. That is a test fixture, not an offline general-purpose language model.

## Workflows

### Verified edits

Feather's primary file interface is the anchor toolset:

```text
read → anchor_grep → replace / replace_within / insert / copy / move → undo
```

Anchors are four-letter identifiers for served lines. Edits verify the current
mapping and return a diff. Shell commands are for builds, tests, Git and inspection;
known shell-based source editing patterns are rejected. This is an editing
safeguard, **not an operating-system sandbox**.

### Skills

Put skills in either location:

```text
~/.feather/skills/<name>/SKILL.md
<workspace>/.feather/skills/<name>/SKILL.md
```

A skill file begins with YAML metadata:

```markdown
---
name: review
description: Review Rust changes for correctness and allocation costs.
---
Read the changed functions and their callers. Check ownership, cancellation,
error handling and tests before recommending changes.
```

Type **`/skill`** to list discovered names/descriptions. Filter the list, then click
or use arrows and Tab/Enter to insert the skill invocation. Add your request and
press Enter:

```text
/skill:review inspect the streaming parser
```

Only metadata is loaded for discovery; the selected instruction body is read
fresh when invoked. Copy a Pi-compatible skill **folder**, including its assets,
into Feather's skills directory. Skills are not bundled with the installer.

### Sessions and rewind

Use **`/resume`** or **Ctrl+Shift+R** to choose a saved session. **`/rewind`** lists
committed turns by their user requests; select with arrows/Enter or a click.
Rewind restores model context and available file checkpoints while preserving
original conversation audit history. Stop an active response before executing
rewind. A fresh session has no committed turns to restore.

File checkpoint coverage is not a substitute for Git or backups. In particular,
external edits made outside Feather's tracked tools may not be restorable.

### Research and subagents

The runtime includes web search/fetch and task tools. Provider-native search is
used where supported, with a public-search fallback. Research results are data,
not instructions. Subagents can run independent tasks with selected models and
tool sets; their records are stored with the parent session. Provider requests
can incur fees, and tool restrictions must be configured for your workload.

### Proxies and rendering

Click **Proxy** in the footer to choose a saved/detected localhost proxy or add
an endpoint. Supported schemes are `http://` and `socks5h://`; **DEL** removes the
highlighted saved entry when you are not editing its address. Selection applies
to the next request. Feather does not modify your VPN, system proxy or upstream
REALITY configuration.

Click **Render** to select CPU or accelerated rendering. A renderer change
restarts Feather with session/draft handoff. Windows acceleration uses **DX12**,
not the legacy WGL path; GPU startup failure attempts a fresh CPU process.

## Keyboard shortcuts

| Action | Default |
| --- | --- |
| Send / queue a follow-up | Enter |
| New line | Shift+Enter |
| Stop response / close picker | Esc |
| Choose model | Ctrl+L |
| Cycle model forward / backward | Ctrl+P / Alt+P |
| Choose effort | Ctrl+Shift+T |
| Cycle effort | Shift+Tab |
| Fold / unfold reasoning | Ctrl+T |
| Fold / unfold tool cards | Ctrl+O |
| New session | Ctrl+Shift+N |
| Resume session | Ctrl+Shift+R |
| Shortcut settings | F1 |
| Toggle fullscreen | F11 |

Picker rows support arrows and Enter. Bindings can be customized in settings.
The footer shows **session · model · effort · TPS** on the left and
**Render · Proxy** on the right. Live `~` throughput is an estimate; final rates
use provider usage when available.

## Headless use

The interactive TUI is retired. The `feather` executable still provides one-shot
and JSONL RPC modes for scripts and subagent integration:

```bash
feather -p "Explain the failing tests"
feather -c -p "Continue the previous task"
feather --mode rpc
feather --help
feather --version
```

In RPC mode commands arrive on stdin and responses/events are written to stdout.
The implementation and integration tests live in
[`rpc.rs`](crates/feather/src/rpc.rs) and
[`crates/feather/tests`](crates/feather/tests).

Environment variables:

| Variable | Purpose |
| --- | --- |
| `FEATHER_HOME` | Override the default `~/.feather` state directory |
| `ANTHROPIC_API_KEY`, `OPENAI_API_KEY` | Provider credential fallback |
| `FEATHER_BASH` | Override the Bash executable used by tools |
| `FEATHER_GUI_RENDERER` | `cpu` or `wgpu` for renderer selection |

On Windows Bash tools require **Git Bash or MSYS2 Bash**. Commands use POSIX
syntax, not `cmd.exe` or PowerShell syntax.

## Build and test

Use a current stable Rust toolchain. On Windows install the MSVC build tools and
Windows SDK; Git Bash is needed for the agent's Bash tools and shell-based tests.
No separate JavaScript build is required.

```bash
cargo build --release -p feather -p feather-gui
cargo test --workspace
cargo check --workspace --all-targets
```

For a smaller local development cache:

```bash
CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0 \
  cargo test --workspace
```

Build the Windows installer with **Inno Setup 6**:

```text
ISCC installer/Feather.iss
```

Output: `dist/Feather-1.0.5-Setup-x64.exe`. See
[installer instructions](installer/README.md) for local deployment details.
Native Windows smoke tests are in `scripts/cpu-smoke.ps1` and
`scripts/renderer-restart-smoke.ps1`; they control isolated synthetic preview
windows rather than the user's live session.

## Architecture

| Crate | Responsibility |
| --- | --- |
| `feather-core` | Agent loop, provider streaming, messages, sessions, snapshots, compaction |
| `feather-hashline` | Anchor maps, verified edits, persistence and undo |
| `feather-cache` | Provider-neutral prompt-cache planning and usage accounting |
| `feather` | Tool implementations, config/auth, skills, CLI/RPC and wiring |
| `feather-gui` | Native desktop UI, rendering, transcript paging and interaction |

The 1.0 review removes deep conversation/tool-schema clones from normal request
construction, caches context token/hash metrics, avoids per-line SSE buffer
shifts, combines audit/metadata commits and reuses GUI buffers. These are concrete
optimizations—not a claim of zero allocations or a fixed total process size.
Provider JSON bodies, active model context, live tool output and graphics buffers
still require memory. See the [function-level review](docs/code-review-1.0.0.md)
and the reproducible synthetic `performance_audit` example.

## Local data, privacy and safety

- Global state defaults to `~/.feather`; project resources use `.feather` inside
  the workspace. Configuration is stored in `config.redb`; sessions use redb
  audit/snapshot stores. Keep these directories out of version control.
- Supported legacy JSON configuration is imported/archived inside redb before
  cleanup. API payloads and serialized redb documents may still use JSON.
- **redb is not encryption.** Credentials and transcripts are sensitive local
  data. Protect the state directory and backups with OS access controls.
- Conversations, requested file content and selected skill instructions can be
  sent to the chosen model provider. Web research contacts external services.
- Agent tools can execute commands and edit files. Use a disposable workspace or
  suitable OS isolation for untrusted tasks; tool policy is not a complete
  sandbox. Review diffs, test changes and retain independent backups.
- Retry policy targets +5/+10/+30/+60 seconds after the initial non-quota failure;
  cancellation interrupts waits. Quota exhaustion is not blindly retried.

## Contributing

Keep changes focused and add regression tests for ownership, cancellation,
persistence and UI behavior. Prefer borrowed data or shared immutable ownership
in hot paths; justify deep clones when isolation genuinely requires them. Avoid
performance claims without reproducible measurements.

## License and acknowledgements

The workspace declares **MIT** licensing. Third-party attribution is collected in
[third-party notices](third-party-notices.md); dependency licenses remain their
upstream licenses. Feather's workflows draw on Pi's coding-agent concepts and
hashline-style editing; it is an independent Rust implementation.
