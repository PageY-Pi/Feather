# Feather 1.0.0

A native Rust coding agent with anchor-verified editing, multi-provider API/OAuth
support, durable redb sessions, lazy skills and a keyboard-first desktop UI.

## Performance and ownership review

- Shared immutable message/tool-schema request context replaces deep clones.
- Incremental token/hash caches and prefix sums remove repeated history scans.
- Linear SSE parsing and FIFO events replace per-line/front-of-vector shifting.
- Raw audit and metadata update in one redb transaction; serialization borrows.
- Compaction borrows its summarized span and copies only the reasoning-free tail.
- GUI suggestion memoization, prefix-buffer reuse and background-worker-only path
  clones reduce redundant per-frame allocations.
- More direct child/audit decoding, model merging, Bash output reuse and retained
  anchor persistence handles avoid temporary copies and repeated database opens.

See [the review](code-review-1.0.0.md) for function-level changes, measurements,
intentional ownership costs and deferred opportunities. Zero allocations/fixed
whole-process memory are not claimed.

## User interface retained

Uniform dotted footer, no Send button (Enter sends/queues), inline `/skill` and
`/rewind` selection, proxy deletion, CPU/DX12 selection, bounded transcript paging
and monitor-adaptive maximum frame rate remain regression-covered.

## Validation

Full workspace suites: 79 app library tests, 92 core library tests, 6 cache
library tests, 161 GUI tests, 24 hashline tests, plus integration/other suites.
`cargo check --workspace --all-targets` passes. Production lib/bin Clippy with
redundant-clone and needless-collect denied passes (other style warnings remain).
Windows native CPU/DX12 close/minimize/maximize and automatic renderer-restart
smokes passed on the release build; final installed version/hashes are verified.

## Distribution

`dist/Feather-1.0.0-Setup-x64.exe` is a per-user Windows x64 installer. It bundles
only feather.exe, feather-gui.exe and third-party notices, never credentials,
config databases or sessions. Unsigned; SmartScreen may warn. Close and reopen
a running old Feather window to activate the installed update.

Final installer SHA256:
`EE09458B8876ECE99F382CA3A525C2399C9F4A4096F1E1556C191D55AA3ACE97`

Native click automation had one intermittent chooser-opening miss; immediate
re-run passed both automatic renderer switches and minimized close. No user
process was killed and no global display/VPN setting was changed.

The root README is the GitHub introduction and setup/workflow guide. Publishing
the repository/release or adding a Git tag is a separate action, not performed
by local compilation/installation.
