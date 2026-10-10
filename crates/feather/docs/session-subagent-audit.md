# Sessions, native children and redb storage

## Desktop sessions

GUI startup discovers conversations for the canonical workspace directory. With
history, it opens a centered pearl/sage chooser; the newest conversation is
selected. Click a row or use search, arrows and Enter. New conversation, × and
Esc offer a fresh welcome. No durable conversation is created before the first
message. `Ctrl+Shift+R` and the footer workspace/session label reopen the chooser;
`Ctrl+Shift+N` starts fresh while idle.

F1 settings includes **Continue latest workspace session on startup** (stored as
`autoResumeLatest`). With no history the welcome is shown directly. Switching is
refused while a run is active. Reopened user, assistant and tool rows are rebuilt
from the saved context. Switching context does not restore project files; all
sessions in the same cwd share that worktree. File rewind is explicit and separate.

## Runtime storage

New runtime history and metadata are redb-only, under `~/.feather/sessions/`:

- `catalog.redb`: workspace discovery cache, readable without opening an active
  session's database in another process.
- `<id>/session.redb`: metadata and append-only raw message audit, including
  parent tool results returned by children.
- `<id>/snapshots/snapshots.redb`: current context and content-addressed turn snapshots.
- `<id>/subagents.redb`: child missions, structured messages, full answers/errors.
- `<id>/tasks.redb`, `checkpoints.redb`, `metadata.redb`: tasks, checkpoint mappings,
  and optional display names.
- Anchor homes contain `anchors.redb`; child anchor homes are isolated by child ID.
- The terminal renderer retains its existing `ui.redb` line store.

No new runtime `.jsonl` transcripts are produced. JSON serialization inside redb
values is an encoding, not a JSONL file. Settings, model catalogs, credentials,
keybindings and other user-editable configuration intentionally remain JSON files.
User project files, task-generated README/TODO mirrors, explicit exports and Git
objects/file-rollback backups are not conversation databases and retain their
normal formats. The RPC stdin/stdout protocol remains newline-delimited JSON;
it does not imply a disk JSONL transcript.

### Legacy migration

Opening an old session parses its complete legacy transcript, imports metadata
and audit rows in one redb transaction, verifies readback, then archives the old
file as `session.jsonl.legacy`. Interior corruption fails without deleting the
source; a torn final crash line is handled explicitly. Raw audit records remain
separate from the current snapshot, preserving rewind/compaction semantics.
Tasks/checkpoint/anchor/name records migrate on access. Legacy backups are retained
for recovery, not appended by the new runtime.

Close old Feather windows before bulk migration:

```sh
cargo run -p feather --example migrate_sessions
```

This verified utility also imports dormant session/task/checkpoint/name/anchor
records. Do not delete legacy backups before validating your histories. Do not
mix an old JSONL-writing executable with the new release during migration.

## Native children

The tools are `subagent` and `subagent_models`. Example:

```json
{
  "task": "Review the parser and return findings only.",
  "label": "parser-review",
  "model": "deepseek/deepseek-flash",
  "thinking": "high",
  "tools": ["read", "anchor_grep"],
  "timeoutMs": 60000
}
```

- Explicit `model` resolves via the live catalog/auth pipeline; only the bare
  model ID is sent on the HTTP wire. Supported thinking levels are validated.
- The Pi allowlist at `~/.pi/agent/pi-subagent/models-allowlist.json` is used when
  present, with `~/.feather/subagent-policy.json` taking precedence. Its default
  model/defaultThinking are respected. `subagent.use_policy: false` opts out.
- `subagent_models` lists authenticated catalog models and their levels, filtered
  by the allowlist. It does not start children.
- `tools` defaults to the parent's active compiled tools except delegation;
  explicit arrays restrict the child, and `[]` grants no tools. Unsupported names
  are rejected. Anchor maps are rebuilt in an isolated child home even for inherited
  file tools. Inherited task tools share the parent's task store.
- `cwd` defaults to the parent workspace and can be specified for a new child.
- Consecutive child calls in a tool batch execute concurrently. Ordinary tools
  remain sequential; parent tool-result records retain original call ordering.
  Children still share physical files if their cwd overlaps: parallel tasks must
  use disjoint writes.
- Results use status/database/id receipts. Answer body is capped at **8192 UTF-8
  bytes / 200 lines**, without splitting characters. Full output stays in child
  redb. Progress is bounded to ~2048 bytes, updated around every 100ms, propagated
  as ToolProgress to GUI tool cards and RPC tool_execution_update events.
- Overall timeout defaults to five minutes, configurable per call. Parent abort
  cancels child provider work including the HTTP-header wait. Timeout/abort/errors
  return failed receipts and persist error state rather than claiming success.
- Every finalized user/assistant/tool message is written to child redb as it enters
  the context, not merely after the run. Partial in-flight token text is progress,
  not a committed message. Child IDs and shared redb handles support sibling runs.

The runtime remains pure Rust/in-process rather than launching Pi/Node children.
This release implements the requested model/thinking/tool selection, parallelism,
bounded results and progress. It is not a claim of complete plugin equivalence:
child resume, opt-in recursive delegation, plugin lifecycle/extension loading and
all configurable plugin collector budgets remain separate work. Unknown `resume`
is not a supported native operation and must not be treated as a continuation.

## Verification

Tests cover startup choices/no premature durable session, reopen row reconstruction,
workspace filtering, raw redb audit and atomic legacy import/corruption preservation,
parallel barrier execution, progress delivery including the last update, ordered
parent redb results, child shared-store isolation, empty tool lists, model/thinking
HTTP request shape, deadline during header wait, bounded Unicode answers/full redb
readback, cancellation, tool-call roundtrip and durable errors. Release UI smoke
used two isolated redb conversations and verified no extra session or JSONL output.
