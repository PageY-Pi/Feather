# Thinking folds, external file rewind and memory audit

## Ctrl+T

`app.thinking.toggle` now folds/unfolds **Thoughts** bodies instead of hiding
thinking cards. Headers remain visible and individually clickable. The global
mode is applied when a virtualized row is next rendered, so offscreen rows obey
the fold setting too. New thinking cards use the current global mode.

## External files

Git workspace checkpoints remain in use for compatibility and in-workspace
changes. Anchor-tool edits **outside the session workspace** now also have
content-addressed redb file history in `checkpoints.redb`:

- Pre-edit baseline (including a nonexistent file) is persisted before mutation.
- Post-edit state is persisted after writes/replacements/inserts/copies/moves/undo.
- Parent turn commits record external states; child agents use parent history.
- `/rewind <turn>` restores those files as well as Git workspace state.
- Restoring a turn before file creation removes that file; forward rewind restores it.
- Files are stored by SHA-256 digest; repeated content is stored once on disk.
- Reopen retains history; divergent future versions are dropped on a new commit.
- Restore verifies all tracked external files first. Later user/other-session
  edits cause a conflict, not silent overwriting. Changed path resolution and
  symlink/directory replacements are rejected. Regular file bytes, read-only
  flag and Unix permissions are preserved.
- Restores use same-directory temporary files and rename. If a file or the Git
  workspace restore fails, already-applied external replacements are rolled
  back best-effort. This is **not** a cross-filesystem atomic transaction; process
  death mid-restore or a rollback failure may require manual recovery.

Scope: regular UTF-8-addressable paths, at most **16 MiB per file**. Anchor tools
already reject binary/oversized edits. This is not a watcher or whole-machine
backup: arbitrary Bash commands modifying external files are not captured.
Directory creation may leave empty directories after rewind. Git itself is
still required for the existing workspace checkpoint path. Old external edits
made before this feature have no reconstructable pre-edit baseline.

## Memory changes

- redb's default cache budget was 1 GiB **per database** (a budget, not necessarily
  resident allocation). All database creation/open paths now use a **4 MiB**
  cache budget, including session, snapshots, runtime records, catalog, config
  and TUI event storage.
- Windows CJK setup previously loaded YaHei, YaHei Light, Deng, SimHei and SimSun
  together: about **72.8 MiB of font bytes** on the measured machine, plus parser
  state and startup clones. Now it loads the first available CJK fallback and
  moves its bytes into the font definitions. Chinese-rendering tests still pass.
- Persistent GUI galley cache reduced from 32 MiB to **8 MiB**. Virtualized row
  heights remain independent of cache eviction (scroll regression tests pass).
- GUI runtime uses **2 async workers**, max 8 blocking workers, rather than one
  async worker per logical CPU. CLI also uses 2 async workers. The measured
  machine has 16 logical CPUs.
- Grep and editing now share one anchor store rather than duplicate mapped files.
- Removed byte-vector and content-string clones when building anchor maps.
- Capability cache application no longer clones entire provider model arrays.

### Measurement

Windows, same offline `FEATHER_UI_PREVIEW=1` fixture, fresh processes, sampled
with `Get-Process` after initialization; no live API requests:

| Build | Working set | Private memory |
|---|---:|---:|
| Existing installed GUI | 322.1 MiB | 314.2 MiB |
| New development GUI | 158.6 MiB | 152.1 MiB |

Approximately **51% less working set** in this fixture. This compares the
existing installed executable with the development build, not two controlled
release builds. Long-session/tool-heavy workloads, graphics driver allocations
and retained transcript/context vary; the figures are not a universal RSS cap.
The user's already-running GUI was separately observed around 395 MiB and was
not closed or replaced during measurement.

Validation includes the full workspace test suite, Chinese glyph tests,
fold-header rendering, external file Session/AnchorStore integration,
forward/backward rewind, reopen/branching, conflict protection and failed
workspace restore rollback. Unix symlink test is compiled/run on Unix only.
