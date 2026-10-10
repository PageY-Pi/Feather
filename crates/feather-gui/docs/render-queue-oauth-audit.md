# OAuth, transcript stability, and follow-up delivery audit

## Confirmed bugs and fixes

- Windows OAuth browser launch used `cmd /c start` with a raw URL. Query `&` separators could become shell commands, losing required authorization parameters. The authorize parameters match Pi's Codex flow. Browser launch now uses `ShellExecuteW` directly, preserving the complete UTF-16 URL, including ampersands and percent escapes. Browser failures surface; no authentication configuration is rewritten. API-key portal launch uses the same helper.
- Every transcript row used to be laid out and painted on every caret/token repaint. Variable-height virtualization now measures only intersecting rows, keeps offscreen spacers, and gives rows stable identities. Scrollbars reserve constant width; scroll animation is disabled. The composer has its own bounded scroll area and a single-height status footer.
- Growing thinking/code text used to reshape settled content. UTF-8-safe stable chunks now retain completed cuts. Offscreen chunks are initially unshaped spacers; only nearby chunks populate the font atlas, at most six completed chunks per widget/frame. Glyph submission is viewport-bounded. Font-atlas identity, scale, and width invalidate cached layouts. A weak atlas guard prevents address reuse without retaining old atlas pixels. Full source text is retained.
- Directory enumeration ran synchronously on caret frames. Workspace listings are now refreshed asynchronously through a two-second cache; build directories remain ignored.
- Streaming event wakeups are coalesced to approximately 30 FPS. GUI event draining has a four-millisecond / 2048-event budget; remaining events schedule another frame.
- GUI considered an inner `RunEnd` to mean the whole prompt future had completed, although core could still consume queued follow-ups. A single background producer now forwards all buffered assistant events before `RunDone`, including successful completion. `running` stays true until that ordered completion.
- Queued user bubbles are inserted on actual core delivery, after the preceding final assistant response, rather than immediately between its deltas. The composer shows the pending count. Follow-ups are separate requests, one at a time by default. Abort/error boundaries do not automatically consume the next prompt. Inputs accepted between core's final empty-inbox check and GUI completion are explicitly restarted; they cannot remain context-only, unrequested messages. Only inputs that fail persistence before delivery are restored to the queue.

## Validation (Windows, 2026-10-08)

- Full dirty-worktree workspace suite: **362 tests passed** (including pre-existing local diagnostic tests).
- Independently exported HEAD plus only the staged task patch: **347 workspace tests passed**, without relying on unrelated dirty files or temporary diagnostics. The existing required `feather-logo.png` source asset was also explicitly tracked; it had previously been hidden by the outer repository's PNG ignore rule.
- New assertions exercise actual focused composer typing, arrow movement, caret-only frames, final-response ordering, queued tool-time prompts, multiple follow-ups, cancellation, and late-completion enqueue.
- Release headless test, 10,000 transcript rows: **2.44 ms/frame mean** over ten frames including warmup; fewer than 100 text shapes including surrounding controls. This is CPU UI construction, not GPU presentation timing.
- Huge thinking regressions use 140,000 CJK characters. First-frame completed-chunk font layout calls, submitted shapes, and visible vertices are bounded; hidden text is not eagerly added to the atlas. No text truncation.
- Native Release smoke, isolated local HTTP provider and temporary home: reopened a 140,000-CJK thinking message and resized the window repeatedly. **80 compositor samples / 4.45 seconds**, GUI CPU **0.266 seconds**, `DwmFlush` median **3.77 ms**, maximum **10.88 ms**, zero samples over 500 ms. This verifies the tested workload did not reproduce the reported two-second systemwide display stalls. It does not establish a GPU-driver root cause or substitute for a presentation trace of the original failure.
- OAuth unit tests verify required Codex query parameters, exact browser-target preservation, and rejection of control characters/non-HTTP targets. Interactive OpenAI sign-in still requires the user's browser confirmation; no end-to-end authentication claim is made from these tests alone.

## Remaining limitations

- Unvisited variable-height rows/chunks use estimates. Their first measurement, resizing, changing thinking visibility, or expanding tools can adjust scroll extent. Warmed caret-only repaints must not change geometry. All original content remains available by scrolling.
- The systemwide stall's exact graphics/compositor cause remains unproven. The confirmed eager-layout/hidden-glyph workload and synchronous-directory I/O were removed, but if the original failure recurs, collect an ETW/WPR graphics trace during it rather than guessing that the driver is responsible.
- Explicit steering still intentionally enters the current task; ordinary Enter and follow-up actions do not. Pending aborted follow-ups remain in the inbox and are not silently run after cancellation.
