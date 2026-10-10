# Function-level review — Feather 1.0.0

## Scope and method

The review follows the runtime by function: request construction, cache planning,
stream decoding, message insertion, durable storage, context replacement,
transcript geometry, menu loading, subprocess output, auth/config transactions
and tool editing. It combines targeted source/caller review, independent
read-only reviews, ownership-sensitive regression tests, Clippy and synthetic
comparison benchmarks. It is not a claim that every line or every platform has
been formally verified. No user conversations or credentials are benchmark data.

Key review surfaces:

| Area | Functions / files reviewed |
| --- | --- |
| Core runtime | `Agent::build_request`, `run_inner`, streaming/retry, compaction; `Session::push/open/rewind_to/fork/commit_turn` |
| Provider streaming | `SseParser::feed/finish`, `ResponseStream::run_handler/next`, OpenAI/Anthropic/Responses/Pi conversions, transport/proxy route selection |
| Storage | `SessionStore::append_raw/raw_messages/bulk_import`, snapshots, child records, `Records::open`, bounded database caches |
| Prompt/cache | `CacheOptimizer::plan`, token estimators, compaction serializer/cut, reasoning exclusion |
| Tools/app | anchor verification/persistence, Bash pipe draining, model merging, config migration/locking, OAuth lifecycle, skills/subagent metadata |
| GUI | frame/composer/footer, suggestion loading/selection, transcript prefix geometry, history paging, software viewport command handling, frame pacing |

## Implemented findings

### High impact

1. **`Agent::build_request`: eliminate deep history and tool-schema copies.**
   Requests and Session now share an immutable `Arc<Vec<Message>>`; tools are an
   `Arc<Vec<ToolDef>>` owned by config and shared by requests. Normal request
   construction no longer duplicates multi-megabyte message payloads. `run_inner`
   explicitly drops the request before `Session::push`, keeping ownership unique.
   A deliberately retained external snapshot uses copy-on-write on mutation to
   preserve correctness; zero-copy cannot override that isolation guarantee.

2. **`Session` / `CacheOptimizer`: estimate/hash each new message once.**
   Append updates cached context tokens, canonical message hashes, total history
   hash and scalar prefix-token sums. Agent thresholds and cache planning consume
   those caches instead of scanning/serializing the full payload each tool turn.
   Compaction/rewind/open rebuild them when history is replaced. Compatibility
   planners keep a default fallback; standalone planner calls still work.
   The caches cost two u64 values per message plus vector capacities, rather than
   duplicate message payloads. This is a deliberate time/memory trade-off, not
   literally zero memory overhead. Hashes/token counts are tested against original
   canonical bytes and the unchanged heuristic.

3. **`SseParser::feed` / `ResponseStream::next`: remove quadratic shifting.**
   Scan complete lines by offset, decode a borrowed UTF-8 view and compact the
   byte buffer once per feed. Queue events with `VecDeque::pop_front`, not
   `Vec::remove(0)`. Provider handler Vec signatures remain compatible through a
   reusable scratch vector. Fatal errors preserve preceding events, discard the
   remainder and reject subsequent payloads in the same chunk.

4. **`SessionStore` / `Session::push`: one audit+metadata commit.**
   Serialize a borrowed RawRecordRef, not a cloned Message, and commit raw row,
   modified time and first-prompt metadata together. Legacy bulk import also
   borrows records. `raw_messages` decodes directly instead of building a whole
   `Vec<RawEntry>` first. On-disk format remains unchanged.

5. **`Agent::compact`: borrow the summarized span, copy only the kept tail.**
   Cut planning estimates original messages while ignoring thinking, with no
   temporary sanitized full-history vector. The summary serializer accepts a
   borrowed iterator; existing summary headers are filtered without a cloned
   intermediate Vec. Kept messages copy only non-thinking blocks. Cancellation
   and ineffective-summary behavior still leave the original context intact.

### Other concrete improvements

- `Message::text_cow` borrows the usual single text block; multiple blocks keep
  the exact prior newline semantics. `iter_tool_calls` avoids temporary Vecs in
  compaction, cut planning and the agent loop.
- `message::json_hash` streams canonical serialization into the FNV writer,
  avoiding a full serialized temporary buffer for hashing.
- Token estimation borrows string arguments and computes object-key statistics
  without collecting/joining a Vec. Tests cover empty/Unicode/whitespace keys.
- OpenAI converts user image arrays only when an image is present. Anthropic
  allocates OAuth tool-name normalization data only for OAuth requests.
- `SubagentStore::messages` decodes directly from redb, eliminating a full Vec of
  transcript strings; `push_line` deserializes a borrowed access guard. Inclusive
  tuple ranges avoid a `key + 1` overflow edge case.
- `AnchorStore` retains its redb handle so repeated operations do not continually
  open/initialize/close the database. Content verification remains unchanged.
- Migrated config access skips the migration write transaction/commit when no
  legacy files exist. The exclusive short-lived OS lock stays: redb's own file
  lock means blindly using shared locks for independent opens would be unsafe.
- Model loading consumes the parsed user Value rather than deep-cloning it;
  builtin providers overridden by user entries are not cloned before replacement.
- Bash drains both pipes to EOF, caps capture and shrinks each decoded pipe
  before combining; short strings are moved, not cloned. Output remains bounded
  and head/tail excerpts retained. The per-pipe cap still requires buffer memory.
- Skill discovery streams directory entries and bounds recursive depth to 64.
  Frontmatter-only discovery and lazy bodies remain unchanged.
- Closed-menu frames no longer allocate cloned root/home paths; clones occur
  only at the background-worker ownership boundary.
- Slash suggestions are memoized by input/metadata revisions and borrowed across
  repeated UI passes; only the chosen entry is cloned for selection. Tests cover
  100 identical lookups building once, plus mouse/keyboard selection behavior.
  Closed menus use an empty key, so ordinary long drafts are not copied into the
  memoization state on each edit.
- Transcript prefix-sum storage is reused across normal layout passes; geometry
  rebases and scroll-anchor behavior remain regression-tested.
- CPU viewport commands move out of the output instead of cloning every command.
- Clippy-identified redundant clones in RPC, OAuth and queued input were removed;
  needless collection in model listing was removed. Session row comparisons
  against usize minimum were corrected to equality. Touched ownership code is
  documented; no global auto-format/refactor overwrote unrelated dirty sources.

## Reproducible measurement

Run the synthetic comparison with an optimized build:

```bash
cargo run --release -p feather --example performance_audit
```

Observed on the release-test Windows machine:

| Synthetic work | Former approach | New approach |
| --- | ---: | ---: |
| 200 history copies/builds, 256 messages / 7,865,234 text bytes | 695.798 ms | 0.040 ms |
| 200 full token rescans vs cached reads | 2,103.209 ms | 0.000100 ms |
| 20 parses of one 10,000-event SSE chunk | 660.793 ms | 13.735 ms |

The new request build includes the cache planner's cached path; the old column
measures the prior deep-copy operation alone, not a reconstructed end-to-end
runtime. All compared outputs/estimates are checked for equality. Extremely
small cached-read timings are timer-floor-scale; do not infer universal speedup
ratios from them. Model/network latency and provider TPS are not benchmarked.

## Costs deliberately retained / follow-up opportunities

- JSON wire-body construction, SSE payload Strings, active context, tool output,
  snapshots and graphics buffers require memory. Shared ownership increments a
  refcount; retaining snapshots across mutation can legitimately deep-copy.
- GUI virtualization is not a total-process constant-memory guarantee. Visible
  pages, an oversized row/current stream, model context and renderer caches are
  necessary exceptions.
- Footer formatting/egui layout still allocates UI text; memoizing every font/DPI
  dependent galley adds invalidation complexity. Only proven redundant copies
  were removed; uniform spacing is preserved.
- Tool-call arguments and finalized assistant messages are copied when an event
  consumer and the durable session genuinely need independent ownership. A
  deeper Arc message/event redesign needs separate API/lifecycle testing.
- `Session::fork` still does per-message persistence; batching a cold path is a
  future optimization, not a reason to risk snapshot/audit correctness in 1.0.
- `AnchorStore::ensure` still reads/compares content to detect external edits.
  A size+mtime-only shortcut can miss same-size changes with unchanged/coarse
  timestamps; it was not adopted for anchor safety.
- Config reads keep cross-process locking and legacy-file presence checks.
  Sharing an unlocked permanent database handle would break CLI/GUI coexistence.
- There remain non-fatal Clippy style warnings in existing code/tests. Strict
  redundant-clone and needless-collect checks pass for production lib/bin targets;
  the workspace is not advertised as fully warning-free or formally audited.

## Validation and release

Full workspace tests, `cargo check --workspace --all-targets`, whitespace checks
and targeted strict Clippy checks were run. Core regressions cover shared request
identity, external-snapshot isolation, cached metrics, canonical hash bytes,
compaction equivalence, split UTF-8/CRLF and 10,000-event ordering. GUI regressions
retain footer gap alignment, no Send button and real skill/rewind selection.
Release-native CPU/DX12/restart checks and installer hash verification are the
final packaging gates. The user's live process/config/session is preserved.
