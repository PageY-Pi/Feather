# Scroll anchoring and minimal feather mark

## Why scrolling twitched

The virtual transcript positioned rows using estimated heights, then replaced those estimates after painting. Lazy thinking/code chunks did the same inside a long row. Measuring an overscanned row above the viewport changed the prefix sum without compensating the scroll offset. Evicting a layout could also revert measured geometry to estimates.

## Changes

- Keep the row intersecting the top of the viewport anchored. Height deltas from rows above it rebase the scroll offset by the same amount.
- Inside a long reasoning/code row, accumulate height corrections from completed chunks above the anchor using their **old** positions, including earlier block changes.
- Discard a sizing pass and repaint with corrected geometry before presentation (at most three egui passes). Ordinary wheel input does not force extra passes; egui's normal smooth wheel/momentum handling remains enabled.
- While the pointer is held for dragging, freeze allocated row/chunk heights and scrollbar extent. New measurements are cached separately and committed with anchoring on release. Clip pending content to its allocated region to avoid overlapping the next chunk/row.
- Prefetch a 1200-point band around the viewport with an eight-chunk lazy shaping budget per text widget/pass. Already measured heights survive galley eviction and atlas invalidation; changing wrapping width or scale still legitimately triggers layout updates.
- Expensive chunk/tail layouts share a global ring/LRU with **128 entries / 32 MiB** caps. Weight accounts for retained text, sections, glyphs, row vectors, mesh vertices and indices. Per-piece references are weak: eviction really releases cache ownership. This is a cache budget, not a claim that the entire process uses 32 MiB: source/session history, lightweight height/range metadata, font atlas, egui's frame cache, and active output meshes remain separate. The original transcript content and TUI ring buffer are not truncated or replaced.
- Chunk metadata stores source ranges and fingerprints, no complete text clones. Repeated reverse scrolling can reuse warmed layouts until normal ring eviction.

## Icon

The new mark is a solid sage feather with a dark contour, a simple quill, and a few interior strokes. There are no facial features, triangular masks, or transparent barb cutouts. The GUI uses one shared embedded texture for toolbar, welcome, and assistant marks, matching the native PNG/ICO assets instead of the old procedural cutout geometry.

`assets/generate_feather_icon.py` generates the PNG, SVG source and native ICO sizes 16/20/24/32/40/48/64/128/256. Pillow is only a regeneration dependency, not part of the application runtime.

## Validation

- Full working tree: **371 workspace tests passed**. Independently exported HEAD plus only the staged task patch: **356 passed**, without unrelated dirty files or temporary diagnostics.

- Actual egui wheel/reverse-wheel events, scrollbar press/drag/release, and long expanded CJK reasoning are exercised headlessly with geometry assertions.
- A deliberately cold estimated prefix is measured above the viewport; the anchored row stays in the same presented position and the offset rebases before the next displayed frame.
- Repeated frozen chunk passes retain the original allocation; release applies the measured correction once. Atlas invalidation/cache eviction does not lose height.
- Ring entry/byte bounds and reclaimability of evicted layouts are asserted; icon texture reuse is asserted.
- Release wheel/drag regressions passed. Release 10,000-row warmup-inclusive UI frame mean: **2.84 ms**, with bounded visible text shapes.
- Native isolated 140,000-CJK resize smoke: 80 compositor samples over 4.99 seconds, GUI CPU 2.625 seconds, `DwmFlush` median 6.30 ms / max 10.28 ms, no samples over 500 ms. This is a smoke test, not proof about every GPU or a trace of the original systemwide stall.

## Boundaries

Unseen content still starts with estimates; the scrollbar's total range can refine after first measurement or after a resize/expansion. The fix preserves the visible content anchor rather than allocating/shaping the entire history. Continuous new streaming content and intentional expand/collapse may change geometry. No credentials/settings/history are modified by these changes.
