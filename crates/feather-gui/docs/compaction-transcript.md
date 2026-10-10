# Compaction transcript behavior (0.1.7)

LLM context and the visible conversation are different stores of data:
- `Session::messages()` is the compacted context sent to the model. Its synthetic
  user-role summary is intentional for provider compatibility, not a user prompt.
- `session.redb/raw_messages` is the original, ordered audit. Compaction swaps
  context without inserting the synthetic summary into the audit.

The GUI now reopens/resumes/rewinds from the **raw audit**, not the model-context
snapshot. Live compaction changes no existing chat rows and emits only the
successful `Context compacted` notice; no summary text/thinking enters the stream.
Reopening no longer shows a summary as a top user bubble or removes original turns.

Resume decodes up to 96 raw records / roughly 1 MiB first. Scrolling near the top
loads earlier pages on a background reader; a `Load earlier conversation` link
also allows explicit retrieval/retry. Each page is bounded by rows and stored JSON
bytes, allowing one oversized record for forward progress. The reader clones an
existing redb handle and does not acquire the Agent mutex while it is running.
Cursors are exclusive sequence IDs, so new messages cannot shift older pages.
A session switch drops the old page receiver, preventing late pages from another
session appearing. Prepending preserves measured heights and visible scroll
anchor; tool calls/results split across pages merge into one original card.

Loaded original text is held for this opened conversation. This is paged loading,
not automatic eviction of already visited history: scrolling through every page
can still consume memory. Text shaping remains virtualized and its galley cache
bounded. An uncommitted/crash-tail audit entry is visible even if excluded from
LLM context, because the visual history is an audit, not model state.

Normal reopen reads the compact snapshot without first decoding the entire raw
audit. The raw-history fallback on corrupt/missing snapshot remains visible and
unchanged. Metadata normally supplies the first-prompt catalog field.

Tests use only synthetic temporary sessions:
- compact context survives reopen while the GUI pages all 250 original prompts,
  with no summary bubble;
- page row/byte limits, zero-limit forward progress, appending-stable cursors;
- cross-page tool call/result joining and old-session receiver isolation;
- prepend preserves a visible row's on-screen Y on the first presented frame;
- existing real live-compaction summary-leak/no-op and same-count RPC regressions.

RPC `get_messages` intentionally remains an LLM-context API; it is not used to
build the native GUI audit transcript. The native GUI uses redb paging directly.
No transcripts, configs or credentials are included in the installer.
