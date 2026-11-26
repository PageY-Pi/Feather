Release 1.4.0: context/memory features, and stop the installer closing a live window

Three features from the context/memory TODO, plus a fix to the update path that
came out of trying to use it.

**Compaction is traceable.** A summary folded old turns into indistinguishable
text: once a turn was dropped, a detail the summarizer judged unimportant was
unreachable even though every original message was still in `session.redb`.
`FoldedRange` now records the message count and millisecond span a summary
replaced, and `summary_message` writes a `<folded-range>` marker naming it and
pointing the reader at `search_context`. 2.2 µs once per compaction.

**`search_context` makes the audit reachable.** The search lives in
`feather_core::ctxsearch` so the tool and the benchmark share one
implementation. It pages newest-first through the existing `raw_page_before` and
stops at `limit`. Two correctness points the first draft got wrong and tests now
pin: a `limit` stop mid-page must still report that more rows exist, and the
pre-filter must never reject a message the full search would match. The
pre-filter earns its place in cost too: building a searchable string for every
non-matching message cost ~460 µs on a 500-row scan against ~340 µs with cheap
rejection first.

**The summarizer input can no longer be forged.** `serialize_messages` marks
messages with a literal `[User]:` prefix and did not escape that prefix inside
message text, so a file or tool result could produce a line indistinguishable
from a real user turn. Escaping applies to user text, assistant text and tool
results, at line boundaries only, and borrows without copying when nothing is
forgeable. Tool-call arguments are already safe because they are JSON-encoded; a
test pins that, since the encoding rather than the escaper is what protects it.

Ruler: four new cases, one a control. `context.page` measures the same storage
read with no matching, and it is what makes search cost attributable — 335 µs of
search is ~309 µs of deserialisation, so matching-side tuning has nothing left to
win. `compaction.mark` (2.2 µs) and `compaction.escape` (24 µs) are far below the
30 ms turn and do not invite optimisation.

Verification beyond unit tests: `--example search_context_smoke` runs the same
function the tool calls across all 30 real session stores here. That is how the
"Database already open" failure was found — a session held by another process.
It now reports as such instead of leaking a redb message, and a test confirms an
open session is searchable in-process because the store handle is shared.

**Installer safety.** `installer/feather.iss` sets `CloseApplications=yes`, which
lets Setup terminate a running Feather to free its files. Correct for a user
double-clicking Setup; wrong for `scripts/install-local.ps1`, which ran it while
a window was live. That run failed with exit 5 and left the installed 1.3.3
binaries untouched, but the risk is the point, not the outcome: an automated
update must not close somebody's window mid-conversation. The script now refuses
when Feather is running from the install directory and names the safe path.

The safe path already existed: `-BinariesOnly` renames the in-use image aside and
writes the new one in its place. Windows lets a running executable be renamed, so
the live window keeps its old build and the new version applies on next start.
1.4.0 was installed that way: disk shows 1.4.0 with hashes verified, and the
running process kept its original PID throughout.

621 tests pass, 0 failed (was 593); `cargo fmt --all --check` clean.
