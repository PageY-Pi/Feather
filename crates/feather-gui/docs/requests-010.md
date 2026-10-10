# Feather 0.1.10 request changes

## Redb-only local configuration

OAuth/API-key provider store now lives in `config.redb` document `auth`, alongside
settings, models, keybindings, compaction-model, subagent-policy, GUI and proxies.
An `auth.lock` serializes refresh-token rotation; `config.lock` is only held during
short redb transactions, never across HTTP refresh. Preserve unknown providers,
unknown document fields and credential metadata. Settings redb has precedence over
stale settings.json; do not regress current values. Workspace overrides are also
`.feather/config.redb`.

One-time legacy JSON migration is still supported as import, not runtime storage.
Original known JSON/migrated files are archived verbatim INSIDE redb and removed
only after the durable commit. Corrupt input aborts migration without deleting it.
No loose JSON config dependency remains. JSON **API wire format**, internal serde
encoding of redb documents, and embedded public model definitions remain; removing
serde_json would break provider/protocol compatibility, not improve storage.

The explicit migration helper was run locally: all config documents present, OAuth
provider count1, values withheld, no root .json/.json.migrated remained. Live old
GUI may recreate JSON until restarted; final migration runs after installation too.

## Compaction and input overhead

256000 ->8500 tokens is a **96.68% reduction (~30:1)**, not low compression.
Numbers are context estimates, not an exact provider-BPE rate; retained recent
turns + summary + file pointers intentionally survive. Summary max output8192;
keep_recent_tokens20000 is a whole-turn planning target, NOT a minimum retained
length. Tool-result shrinking can reduce it much further. Do not equate model
reasoning output tokens or cache reads with final summary size.

Assistant Thinking is now excluded from summary input and from kept compacted
context. Raw audit thinking stays in redb and visible history. Summary model's
Thinking output was already not used (summary_msg.text only). Previous summary
is inserted exactly once, no duplicated source copy. Recent tool call/result
integrity and final answer survive.

Metadata-only audit on this workspace before adding skills:
- base instructions829chars ≈184tokens
-18tool schemas9600chars ≈2023tokens
-0skills0tokens
-static total≈2207tokens, BEFORE workspace/AGENTS/SYSTEM instructions, conversation
and attachment content. Estimator is heuristic; provider usage is authoritative.

Skills discovery reads bounded YAML frontmatter only. Copy the whole Pi `skills`
folder to `C:\Users\Page\.feather\skills` (not loose files in .feather root).
`/skill` discovery is fresh; `/skill name request` reads the selected body fresh.
Existing agent system metadata refreshes on new/rebuilt session; it never embeds
all skill bodies. Invalid/duplicate metadata produces diagnostics; relative assets
stay next to their SKILL.md. `disable-model-invocation` remains honored.

## Hiddify

Read-only listener enumeration found Hiddify PID8892 mixed HTTP/SOCKS on
`127.0.0.1:12334`; both greeting/CONNECT probes succeeded. Feather selected
`socks5h://127.0.0.1:12334` in redb. REALITY is upstream; Feather does not consume
the remote REALITY subscription/keys/config. Discovery includes12334 and saved
custom local ports. No Hiddify/VPN/system setting was changed.

## GUI/history

- New live conversations are offloaded after durable audit boundaries, including
  during long tool runs. UI high-water256rows/2MiB ->96rawrows/1MiB tail. Current
  stream/queued prompt kept; original older rows reload from redb. At bottom
  release old pages/galleys. Never replace LLM context with UI paging state.
  One oversized message/current stream and near-viewport page buffers are bounded
  exceptions; not a claim that the entire process has fixed memory.
- Footer left: session · model · thinking · TPS, all left aligned. Right:
  Render CPU/GPU · Proxy. No token counter. Truncate identities rather than wrap.
- Monitor queried on native startup and periodically while active. Maximum FPS=
  refreshHz-1 (165Hz ->164FPS); still demand-driven, NOT idle164FPS polling.
  CPU host uses native WaitUntil; GPU cap waits before submitting work.
- `/rewind ` dropdown uses identical command cards and keyboard/hover behavior,
  lists snapshot turns by corresponding user request. Labels loaded asynchronously,
  latest128turns, filter request text or turn id, Enter selects/executes.
- Proxy chooser DEL removes highlighted saved endpoint, except while address
  text field is editing. Removing current endpoint returns to Direct.
- Retries scheduled at initial-error+5s,+30s,+60s, accounting for elapsed request
  time. Three retry maximum, quota excluded, abort/backoff responsive.

## GUI-only frontend

Removed active feather-tui workspace/dependency/module/terminal loop/theme code.
Retired files contain only tombstone notes. Plain `feather` launches sibling GUI
before any model/session is created; noninteractive prompt/RPC bridge retained for
subagent automation and compatibility. Pi keybinding action ids retain historical
`tui.*` spellings as data compatibility, not a terminal frontend.

## Release validation

Final workspace tests passed:79feather library,85core library,151GUI tests
plus integration suites. Check/whitespace passed. Native release CPU/DX12
maximize/minimize/input/close/header-X, renderer automatic restart and minimized
close with60s blocking worker all passed. Plain launcher preview passed.
Monitor native report165Hz =>164FPS cap. Root legacy config files absent after
final explicit migration; credentials withheld in every diagnostic.

DeepSeek image review of this iteration requested additional header/compact-footer
polish (REVISE). Compact labels/gaps/contrast were improved and geometry tests
passed; no new visual PASS is claimed for0.1.10. Previous0.1.9PASS is historical.

Workspace tests/check and native CPU/DX12 close/restart smoke run before packaging.
Installer allowlist remains two release executables +third-party notices; never
user redb/credentials/config/diagnostics. Live GUI preserved, needs reopening.
