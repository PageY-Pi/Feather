A measurement pass over the turn boundary, driven by a new benchmark harness that ships in this build. Every claim below is a number the harness produces, and three of the five fixes were kept only after the measurement supported them.

The work itself was run as an RSI loop: build the ruler, audit the hot paths against it, apply one change, re-measure, prove correctness, repeat. Two candidate optimizations were reverted because the benchmark could not show a gain, and one was reverted because it broke a liveness contract.

### The ruler

`crates/feather-bench` is a plain binary crate — no Criterion, no new dependencies — that times nine cases and writes diffable JSON:

```
feather-bench --iterations 15 --json bench-baseline.json
feather-bench --only session.commit --iterations 15
feather-bench --probe          # sub-step attribution
```

Every case asserts a falsifiable invariant *in the same batch it times*, so a change that makes a path faster by changing what it does fails the run that reported the speedup rather than quietly shipping. `peak_rss_kb` is sampled per case, so memory is tracked next to runtime instead of in a separate exercise.

`scripts/bench-gui.ps1` covers the fourth dimension. It runs the real GUI in its synthetic-preview mode and reads the frame costs the app measures for itself, so rendering latency is not a guess about which code ran.

Reference numbers on this machine (median of 15 iterations):

| case | cost | what it covers |
| --- | --- | --- |
| `session.commit` | **29.6 ms** | one full turn boundary |
| `records.open` | 10–11 ms | opening a redb database |
| `grep.anchor` | 9.1 ms | anchor-mapped search |
| `snapshot.full` | 2.4 ms | serialise a 400-message history |
| `checkpoints.grow` | 2.3 ms | one checkpoint commit |
| `redb.commit` | 2.2 ms | a bare durable write transaction |
| `anchors.read` | 2.4 ms | mapping a 1500-line file |
| `cache.plan` | 135 µs | cache breakpoint planning |
| `tokenizer.message` | 148 ns | one message's token estimate |

GUI frame cost: **6.7 ms** mean, of which **5.6 ms is rasterisation**, against a 33.3 ms budget at 30 FPS.

### What the ruler found

Measuring first changed what got fixed.

- **Serialising the conversation was never the cost.** A bare redb durable write is 1.96 ms and a full 400-message snapshot was 2.04 ms, so the entire serialisation path accounted for about 4% of a snapshot — the incremental-snapshot work in 1.3.2 was addressing a real but small slice.
- **Checkpoint metadata does not grow with session age.** `checkpoints.grow` commits at turn 200 at the same cost as turn 1 (ratio 1.0x), because `commit` retains only versions up to the current turn. The suspected superlinear growth did not exist, and the fix it would have justified was not written.
- **A probe over-reports when its steps each open a database.** Sub-step attribution that constructs a fresh instance per step charged a redb startup to every step, making each look 3–10x more expensive than inside the real path.

### Performance

`session.commit` went from **71.99 ms to 29.6 ms — 59% faster**. The gains, in the order they landed:

- **Opening a records database no longer writes.** `Records::open` ran an unconditional durable write transaction on every open, so merely reading `checkpoints.redb` paid a redb commit. A read on an already-open handle costs 0.001 ms against 20.05 ms for the open-with-write, and `records.open` dropped to 10.02 ms. `get` now maps a missing table to `None` instead of failing, and creation happens only when the file does not exist.
- **The checkpoint database is opened once per turn, not twice.** `commit_turn` called `tracked_paths()` and then `commit()`, and each opened the session's `checkpoints.redb` independently — two redb startups (~5.9 ms each) on every turn boundary. `FileSnapshots` now caches its handle in a `OnceLock`, and `commit_turn` shares one instance across both calls. Worth 10.6 ms.
- **The session catalog handle is reused instead of reopened.** `register_catalog` reopened and re-registered the catalog database every turn. `Session` now holds a `Weak<redb::Database>` and reuses it, reopening only if another session in the process has closed it. Proven by A/B (`git stash` of the two changed files): **54.7–56.0 ms without, 45.9 ms with — 16%**.
- **The catalog is refreshed on a schedule, not on every turn.** The catalog is a rebuildable index: the first prompt registers the session so it is discoverable mid-turn, and `list_sessions` reconstructs any missing row from the session directory. A durable commit per turn was therefore buying only ordering freshness, at **18.1 ms of a 45.4 ms turn** — measured by running the same benchmark with the write disabled, 45.38 ms against 27.29 ms. Turns inside a 60-second window now skip the write: **45.4 ms → 26.05 ms**.

The scheduling change trades catalog *freshness*, never existence — a session is registered by its first prompt, and discovery re-registers from disk regardless. Two tests pin that: one asserts a commit inside the window leaves the timestamp untouched while the session stays listed, and one asserts a stale catalog is refreshed and the recorded timestamp advances.

### Correctness

`cargo test --workspace` — **592 passed, 0 failed** (584 before; six new tests: four for the database-handle lifecycle, two for the catalog refresh contract). `cargo fmt --all` clean.

Two things were reverted rather than shipped:

- **A strong-reference database cache.** Bounding open databases at 16 with LRU eviction broke `session_catalog::delete_session`, which detects liveness by trying to open a session's databases: a held handle made every closed session look active. It also produced cross-test `Database already open. Cannot acquire lock.` The weak-reference design is what keeps the file lock releasable once no session needs it, and weakening that contract was not worth the cache.
- **Catalog open-vs-create.** Real, not measurable — no gain the harness could distinguish from run-to-run noise.

One defect was found and fixed during this work: reusing the catalog handle with `if let … else` self-deadlocked, because the `MutexGuard` from the `if let` scrutinee stays alive through the `else` arm that re-locks the same mutex. The handle is now taken out of the lock before the branch.

### Also in this build

`Cargo.toml` moves to 1.3.3, and `README.md` plus `installer/feather.iss` carry the same version so the installer cannot ship a stale label.

### Install

`Feather-1.3.3-Setup-x64.exe` is the per-user Windows x64 installer; no administrator rights are needed. It is unsigned, so SmartScreen may warn. Close and reopen a running Feather window to activate the update — a running GUI keeps its previous image. Credentials, config databases and sessions are never bundled.

### Assets

- `Feather-1.3.3-Setup-x64.exe` — per-user installer, no admin needed.
- `feather-1.3.3-windows-x64.exe` — CLI binary.
- `feather-gui-1.3.3-windows-x64.exe` — GUI binary; the GUI expects the CLI beside it.

### SHA-256

```
676C72FEC34FA932FC61FE70070A1599E3E2CF2ADB7E09233404E5D8F814FCAD  Feather-1.3.3-Setup-x64.exe
B95608C66CBBD3614D3516E973DEF3D6C172AAE2C2F1FE7D1ABA4260BA7AB7A6  feather-1.3.3-windows-x64.exe
E6B2EC9FBE2DD7D438606CA4900DB17BBF5826F946299AC940A7337BC80F5517  feather-gui-1.3.3-windows-x64.exe
```
