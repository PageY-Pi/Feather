# Request handling and UI follow-up — 0.1.8

## Retries
One initial provider request plus up to three retries: **5, 10, 30 seconds** after
successive errors. Billing/quota/usage-limit exhaustion is terminal, ordinary rate
limits/network/server errors retry. Cancellation interrupts stream wait/backoff.
Retries repeat only an unfinished provider response, never executed tools. A
ResponseReset event discards the failed live response in GUI/TUI/RPC; failed
usage remains accounted. The CLI prints a discard/retry delimiter because it
cannot retract text already written to stdout. Quiet compaction stays private.
Error after the fourth attempt remains an error, not a successful empty answer.

## Skills
Feather already had Pi-compatible resources. GUI slash parsing had not connected
it. `/skill` now opens a metadata chooser; `/skill name request` and
`/skill:name request` load instructions lazily. Discovery reads bounded YAML
frontmatter, not whole skill bodies. Invocation reads the latest file and strips
frontmatter. The system prompt lists descriptions/paths; the model uses `read`
only when relevant. See `crates/feather/docs/skills.md` for roots/validation and
local Pi source parity. Hidden `disable-model-invocation` skills remain explicitly
invokable. Skill content is project instructions, not a grant of shell permissions.

## Proxies
`Proxy: Off` replaces the working/ready footer status. Click it or use `/proxy`.
- Startup scans localhost common ports: 7890,7891,7897,1080,10808,10809,8080,8118,8888.
- Protocol probes are bounded and use SOCKS5 greeting / HTTP CONNECT to loopback
  port 9 only; no external scan, VPN/display/OS settings changes.
- Unseen endpoints and selected route persist in the `proxies` document of
  config.redb. HTTP and SOCKS5 are supported. Manual endpoints can be added.
- URLs with credentials/path/query/fragment are rejected; the catalog has no
  proxy passwords or API keys. Endpoint list cap 128.
- Default **Off/direct** explicitly disables environment/system proxy inheritance.
  Selected routes apply to the next model request, even in an existing Agent.
  An in-flight request is not silently rerouted. Switch during retry backoff to
  use the new route on the next retry; stop/re-submit to replace an in-flight one.
- A saved auto-discovered localhost proxy absent on startup scan falls back to
  direct. Manual endpoints are not cleared by a scan of only common ports.
- Auth clients and native web search also use the selected route. Web fetch keeps
  URL/private-network/redirect checks; proxied DNS cannot be pinned by Feather
  (the proxy controls remote resolution). Explicit proxy endpoints are trusted
  transport, not fetched URLs. No automatic fallback on every transient request
  error: this could silently expose traffic the user intended to proxy.

Socket regression verifies one existing provider sends through a synthetic local
proxy, then directly to a synthetic origin without recreating the Agent/provider.

## Runtime errors
Tool failures already go into LLM context as `is_error` tool results; a test
checks the next actual request and continued correction. Internal git checkpoint
errors were another failure path. Unsafe paths are skipped before git add and
marked `[ILLEGAL_COMMAND]`; ignored/non-file tracker candidates are excluded.
Git uses a separate temporary index and never mutates the user's stage/HEAD.
Checkpoint failures are nonfatal to durable chat snapshots, surfaced as notices
and a bounded `<feather-runtime-diagnostic>` context record, also retained on
reopen. The GUI renders that record as a notice, not a user bubble. Rewind still
fails loudly if the requested file restore is unavailable; it never pretends
files were restored. This is not a complete security sandbox for arbitrary Bash.

Known prohibited Bash/RPC shell operations are preflighted before spawning the
whole chain: file-output redirection (excluding null/fd sinks), in-place text
edits, obvious inline script file writers, and destructive git reset/clean.
Failures return `[ILLEGAL_COMMAND]` normal error results, not Agent aborts.
Ordinary `git add` is still allowed. This conservative gate does not claim to
parse or sandbox every shell expansion/unknown executable. Synthetic integration
proves a command before a prohibited edit in the same chain is not executed.

## UI / history
- Intentional divider between conversation and input restored. GPU opaque canvas
  prevents a second line caused by antialiased section edges.
- Changing TPS digits wrapped the compact footer and moved composer by 16 points.
  Counter widths and token slots are now reserved. 100/125/150%-scale regression
  checks idle/live/caret frames for drift; proxy footer has constant geometry.
- Older raw audit pages above the viewport are removed while scrolling down,
  leaving a two-page near-viewport buffer. At bottom all loaded older pages are
  released. Latest resume page/live entries are kept. Cursors permit reload;
  stable row IDs preserve viewport position and boundary tool cards.
- Eviction drops row Strings and the bounded galley ring; egui's one-frame caches
  expire normally. The independent LLM context is not removed by UI scrolling.

## Validation
Full workspace test/check passes, including retry/abort/no-quota, lazy resources,
proxy route/socket persistence, context diagnostics, and paging/eviction tests.
Native CPU/OpenGL preview/restart/close smoke is repeated before deployment.
0.1.8 vision review was unavailable (provider connectivity/usage-limit), so no new
vision PASS is claimed. Diagnostic PNGs are synthetic and not installer payload.
Build/cache cleanup is performed only after installer construction.
