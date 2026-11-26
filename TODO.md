# TODO

Managed by feather task tools — done tasks leave this file.

_GUI is the primary interface. The legacy TUI remains for compatibility._

## GUI 转向(2026-10-08)

- **决策:放弃 TUI,转向 GUI(egui/eframe)。** `feather-gui` crate 已落地:聊天气泡流、
  侧栏(provider→model 树 + 会话信息)、顶栏(模型/思考级别/中断/新建会话)、
  羽毛品牌资产(gpt-image-2 生成,嵌入窗口图标与侧栏)。
- **快捷键 1:1 移植 pi**:named-action 注册表(全部 action)+ 默认表(含 Windows 变体)+
  `~/.feather/keybindings.json` 覆盖(空列表=禁用)+ 设置面板(查看/重载)。
- **模型选择**:目录(provider→model)选择即写 `settings.model`("provider/id")并持久化;
  ctrl+l 选择器、ctrl+p / alt+p 循环;thinking 级别循环(shift+tab)并按模型 clamp。
- **OAuth 登录**:PKCE + localhost 回调 + 浏览器拉起;token 存 `~/.feather/auth.json`
  (pi 同构:type/access/refresh/expires/accountId);openai-codex + anthropic 两个 provider。
- [x] 后续:OAuth access token 接入 provider 运行时(codex 走 ChatGPT backend base URL,
      bearer 注入),使 oauth 登录的账号直接可跑对话。
- [x] continue；清理target文件夹；全面美化feather UI、字体以及会话栏工具调用等卡片，设计关键词：羽毛主题、Q版、简洁、柔和，文字显示清楚且与所在的背景具有较高对比度，布局合理，避免明显的分界线（淡化），保持纯rust编写；将状态栏放到输入框下面去并增加流式输出TPS监控；除login按钮可保留外其余按钮全部清除；通过调用模型视觉对界面设计进行全面检查并持续迭代设计直到满意 _(2026-10-08 03:07:17)_

## 2026-10-08 · GUI restyle and runtime verification

- [x] Z.AI / Z.AI Coding CN API-key login (pi-compatible auth.json).
- [x] Request-time credential resolution, locked atomic OAuth renewal, Responses/Codex and Radius routing.
- [x] Warm pearl/sage/lavender GUI, Segoe UI + CJK fallback, pure Rust procedural chibi feather.
- [x] Only Login remains as an action button; model/login/settings use keyboard selection and documented shortcuts.
- [x] Status below composer, honest live estimated TPS and final provider-token rate.
- [x] Tool cards: call-ID matching, arguments, completion/error state, elapsed time, safe UTF-8 output truncation.
- [x] Visual review: xhyapi/gpt-6.1-sol, overall 9.2/10, no blockers; final login scrim/centering improved afterward.
- [x] GUI 28 tests; workspace suites green; release GUI/CLI deployed to ~/.feather/bin and smoke-tested.
- [x] cargo clean removed 77,230 build files / 40.4 GiB; target/ and temporary review scripts removed.

Note: the pi named-action registry is available, but not every terminal-only action
has a native GUI equivalent. The original all-shortcuts task remains open rather
than claiming full behavioral parity.
- [x] 以后rust项目默认不追踪target文件夹；UI不错，小问题修一下：目前模型选择界面只能快捷键，建议改成点击输入栏下方的模型栏也能打开选择界面；选择界面没有退出按键，esc快捷键也没用，且不应该显示没有登录的oauth供应商提供的模型（反正也用不了）；thinking对应的effort滑动界面也没了，加回来，风格与其他UI保持一致；另外现在feather的Q版图标更像个叶子，改一下 _(2026-10-08 05:04:06)_

- Picker follow-up: footer model/thinking mouse entries; native × and Esc dismissal;
  OAuth eligibility shared by picker/cycling; supported-level effort slider;
  asymmetric fringed feather mascot with exposed quill. GUI 33 tests and feather
  library 53 tests passed; isolated screenshots visually approved; release deployed.
- Global Git excludes: C:/Users/Page/.config/git/ignore contains target/;
  future Rust artifact policy recorded in ~/.pi/agent/AGENTS.md.
- [x] 给火山引擎的glm-5.3-flash模型发消息返回A little snag provider: provider returned error 400: {"error":{"code":"InvalidParameter","message":"A parameter specified in the request is not valid Request id: 021791441036 _(2026-10-08 06:31:05)_
- [x] continue _(2026-10-08 06:50:41)_

- Volcano GLM 400 fix: GUI now calls Agent::prompt for the initial submission,
  not queue_follow_up + run (which sent a system-only request before draining input).
  Reproduced InvalidParameter with synthetic system-only input; adding a user
  message succeeded. Live catalog/auth/provider call returned OK. GUI 35 tests
  and workspace suites passed, including submission order and follow-up regressions.
  Release rebuilt, installed, and restarted; model configuration unchanged.
- [x] 输入测试消息后，有窗口闪烁，修复该问题；另外working需要更显眼的动画 _(2026-10-08 07:07:37)_

- Window flashing: shared CREATE_NO_WINDOW process constructors applied to
  Git checkpoints, bash, RPC shell/termination, registry queries, and browser
  launcher shells. Intentionally interactive external editors remain unchanged.
- Working: fixed-size rotating orbit badge, pre-response waiting card, streaming
  and tool activity indicators (~30 fps, no blinking); reserved footer height
  eliminates activity-induced composer jumps. GUI 38 tests and workspace passed;
  Windows probes verified both sync/async children have no console window and
  preserve output/exit status. Release GUI/CLI deployed and GUI restarted.
- [x] 目前在同一工作文件夹下的不同会话是如何保存的？要不要在启动时做一个会话选择界面？还是说就直接一个会话到底？另外确认一下feather的subagent行为逻辑与目前pi使用的pi-subagent-feather完全一致且运行正常，无jsonl文件产出，所有结果保存至redb中 _(2026-10-08 07:34:31)_

- [x] Audit session storage and native child behavior against installed
  pi-subagent-feather 0.2.0. Findings: GUI starts new sessions; parent/task storage
  still JSONL; children use memory sessions + redb, but are not plugin-equivalent.
  Report: crates/feather/docs/session-subagent-audit.md.
- [x] Fix child catalog-prefix wire model, millisecond-only ID collision risk, and
  provider-stream-error success classification. Seven child integration checks
  and workspace passed. Synthetic live GLM child returned OK, redb readback passed.
- [x] Implement workspace-scoped GUI startup/reopen chooser: continue latest,
  recent sessions, new conversation; avoid durable empty sessions before choice.
- [x] Implement native subagent/subagent_models behavioral parity: policy, model/
  thinking/tools/cwd, overall timeout, bounded output/progress,
  parallel scheduling and continuous child transcript durability.
- [x] Migrate main sessions, anchors, checkpoint mappings and task tracking to redb with validated legacy import; never discard old transcripts.
- [x] 按照2中要求实现启动会话选择界面，要求UI与主界面风格一致；按照pi插件逻辑实现模型指定、thinking设置、工具指定、并行以及有界结果和实时进度；所有feather保存文件无jsonl，全部进redb，子进程返回进入父会话redb记录 _(2026-10-08 07:57:11)_
- [x] Implement no JSONL redb tasks and RPC transcript adapter. ONLY crates/feather/src/tasks.rs, rpc.rs, NEW storage.rs (register lib.rs if needed), Cargo.toml add redb.workspace if needed, tests/e2e_tools _(2026-10-08 08:10:27)_

  Pi extension lifecycle and configurable collector budgets.
- Storage scope: all new conversation/task/anchor/checkpoint records are redb;
  user-editable configuration/auth remains JSON and user project files/exports
  retain their normal formats. Legacy backups are kept until verified.
- [x] 注意到feather中运行bash的失败率很高，比如cd /c/Users/Page/.feather/sessions && pwd && ls | head -3就失败了，分析当前目录下的feather会话历史，找出原因并解决；另外feather在工具卡中无法正常显示中文汉字，解决该问题；完成后更新exe _(2026-10-08 10:17:17)_
- [x] feather isn't working properly after update, seems like the shortcut to it is broken, fix it _(2026-10-26 13:24:10)_
  The **Open Feather here** key pointed at `%TEMP%\feather-114-verify\feather-gui.exe`
  — a release check ran `--install-context-menu` from a throwaway copy, and the
  temp directory was then deleted. `feather` on PATH also resolved to a stale
  0.1.0 `cargo install` shim that shadowed the 1.1.4 install. Fixed on this
  machine (menu repointed, install dir moved to the front of PATH, shim removed,
  Start-menu/desktop shortcuts recreated) and in the product: a temp copy now
  refuses to register the menu, a real start repoints a stale entry at itself,
  and the installer prepends `{app}` to PATH instead of appending it.

## Open — context and memory

Ideas under evaluation, not commitments. Sizes are the author's estimate of the
change, not a schedule.

- [x] **Make compaction lossy-but-traceable.** Done: `FoldedRange` records the
  message count and the millisecond span each summary replaced, and
  `summary_message` writes a `<folded-range>` marker naming it and pointing at
  `search_context`. `compaction.mark` measures 2.2 µs — once per compaction, so
  it never registers against a turn.
- [x] **`search_context` tool over the raw audit.** Done: `feather_core::ctxsearch`
  reads the audit through the existing `raw_page_before` paging, scanning
  newest-first and stopping as soon as `limit` is met. Registered unconditionally
  in `wiring::build_registry`. Two things the implementation had to get right:
  a cheap pre-filter rejects non-matching messages without building a string
  (A/B: ~460 µs → ~340 µs on a 500-row scan), and a `limit` stop mid-page still
  reports that more may exist. Measured at 335 µs, of which ~309 µs is storage
  deserialisation — the `context.page` control (164 µs for 256 rows) is what makes
  that attribution possible rather than guessed.
- [x] **Escape role labels before flattening for the summarizer.** Done:
  `escape_role_labels` prefixes a backslash on any content line that would read
  as `[User]:`, `[Assistant]:`, `[Tool result]:` or `[Assistant tool calls]:`,
  applied to user text, assistant text and tool results. It borrows and does no
  work when nothing is forgeable. `compaction.escape` measures 24 µs over 400
  messages with 20% adversarial content. Tool-call arguments are serialised as
  JSON, so a newline there is already escaped — a test pins that, because it is
  JSON encoding rather than the escaper that keeps that path safe.

- [x] **Session storage is bounded at the last 10 user instructions.**
  The previous entry correctly diagnosed this as a retention policy rather than leaked
  rows, and left the decision open. The decision: keep everything back to the start of
  the tenth-newest instruction, drop the rest.

  **The first implementation used the wrong unit and had to be redone.** It bounded
  *snapshot turns* at ten. A turn is one model round trip — `commit_turn` runs after
  every assistant message in the tool loop — so on the real 2372-turn session that
  covered roughly **one-seventieth of a single instruction**. Measured, on that
  session: 32 instructions across 2372 turns, ~74 turns each. Ten turns kept the
  newest sliver of one instruction and discarded the rest of the window the user
  asked for.

  The unit is now a user instruction, recognised as a `User` message with no
  `tool_call_id` that is neither a compaction summary nor a runtime diagnostic — the
  same rule the audit trail uses. `Session` records the turn each instruction starts
  on, and every pruning path takes its floor from that list, so one instruction's
  dozens of turns survive or fall together and a rewind lands on a real instruction
  boundary instead of mid-loop.

  Measured on a copy of that session, at both units:

  | window | snapshots kept | MiB | share |
  |---|---|---|---|
  | 10 turns (wrong unit) | 10 | 7.7 | 8% |
  | 10 instructions (asked for) | 777 | 28.6 | 30% |

  The instruction window keeps ~77× more history than the turn window, which is what
  the original number was silently costing.

  Three findings worth keeping, because each one misled a first attempt:
  - **Pruning without compaction saves nothing.** redb holds the file's high-water
    mark, so `vacuum` reclaiming chunks is invisible on disk until compaction. Both
    steps are required, which is why `compact_file` exists and why compaction needs
    an exclusive handle (drop the store, clear the `Weak` cache entry, reopen).
  - **A synthetic test proved the opposite of the truth.** The first version of
    `prune_vacates_chunks_only_the_dropped_turns_referenced` used a growing prefix
    and measured 21 → 21 chunks, because the newest snapshot references *every*
    message in its list. Chunks only become reclaimable when the newest context is
    genuinely smaller than the old ones — which in practice is compaction's doing.
    The test was rewritten around that shape, and says so.
  - **A probe that assumed contexts hold all prior instructions was wrong too.** A
    real session held at most four instructions in any single snapshot, because
    compaction had already rewritten them away. Instruction boundaries cannot be
    recovered by inspecting contexts; they have to be recorded.

  The boundary list is persisted in the snapshot store's own `instruction_turns`
  table, so a resume keeps its window. Two tests pin the parts that would silently
  regress: `one_instruction_spanning_many_turns_is_not_cut_in_half` (counting turns
  instead of instructions fails it) and `the_rewind_window_survives_a_reopen`
  (dropping the persistence fails it — the earlier looser version of that test
  passed with persistence deliberately broken, so the bound was tightened until it
  failed).

  Applied to all three stores that grew, sharing `rewind_floor()`: message snapshots
  (`prune_before` + `compact_file`), per-file versions and their content-addressed
  blobs (`FileSnapshots::prune_before`), and expired `refs/feather/.../turn-N` refs
  in `gitckpt`. Each also keeps the turn the live pointer names, because resume reads
  that pointer and pruning it would turn a rewind into a corrupt session.

  Deliberately not done: no age- or byte-based horizon, only instruction count.
  Instructions are the unit `/rewind` speaks. If ten instructions of fat tool output
  ever get too large, the ceiling is context size, which is a different problem with
  a different fix.
- [ ] **`session.commit` costs ~29 ms per turn.** The slowest write path in the
  bench. Part of it is `flush_audit`'s durable commit and part the
  `file_ops(&self.messages)` scan over the whole history each turn. Worth
  profiling before changing: a faster commit that silently stops persisting
  audit rows would be a correctness loss, and the bench invariant only checks
  the message count.
- [ ] **Evaluate persistent memory — leaning negative.** See the research note
  below. The short version: memory that stores *inferred judgments* inherits
  their errors and carries them across sessions with no expiry, while memory
  that stores *verifiable pointers* (paths, commands, test outcomes) degrades
  safely. Feather already has the safe variant in `AGENTS.md`.
- [x] **Static prompt audit (the first layer of a prompt ruler).** Done:
  `feather::prompt_audit` checks the shipped prompt in milliseconds, so an edit
  gets a verdict without a benchmark run. Four gates, each with a case that
  reaches it: token budget (260, currently 196 with 64 free), dead tool
  references, duplicate rules (significant-word equality, so a reordering still
  counts), and verbatim containment. Tool names are read from the live registry,
  never listed in the module — the first version of the test carried a hand-
  written list and immediately produced six false positives.
- [ ] **Falsifiability is not mechanised, on purpose.** The mechanical versions
  ("must contain an identifier", "must not use a manner adverb") either fire on
  sound rules or degrade into a word blacklist, and a prompt bent to satisfy a
  linter is worse than one a human reads. The check worth adding later is "every
  rule has a scenario where it fails", which needs the behavioural layer below;
  a heuristic over wording would be guessing. In the meantime one rule was
  rewritten by hand: `Keep responses tight.` could never be observed to be
  broken, so it became "Default to prose: one short paragraph per change, lists
  only for real lists", which a test could check.
- [ ] **Behavioural A/B (the second layer) — not started.** The shape: script a
  `FauxProvider` response, run the same scenario against two prompts, assert on
  which tool the model reaches for. That tests whether a prompt *parses and
  steers*, which is the part that is decidable without a live model. It does not
  test whether the prompt is *good*; that needs real traffic.
- [ ] **Do not build a Terminal-Bench-shaped ruler for prompts.** Measured
  budgets for TB 2.1 (89 tasks, all `task.toml` read): 42.2 h of agent timeout,
  83.6 h with verifiers, and the leaderboard requires 5 trials — ~28 h serial, or
  4–5 h at 8-way concurrency. At that cost a prompt change gets one data point a
  day and the model's own variance is the dominant term. TB is acceptance, not
  iteration.

## Tool surface (2026-11-24)

Measured, not reasoned. `--example tool_failures` and `--example tool_misuse`
read all 30 real session stores and classify every tool result by cause; keep them
and re-run after any tool change.

- [x] **Merged `replace_within` into `replace`.** The evidence: of 5047
  `replace_within` calls, **1695 failed on a missing anchor field** — and 1685 of
  those passed `remove_from`/`remove_to`/`replacement_lines`, i.e. the model
  reached for `replace` and got a schema error from the sibling it picked by
  mistake. The two tools had disjoint anchor names (`remove_*` vs `replace_*`) for
  the same concept. `replace` now takes either spelling, and which mode runs
  depends on whether `replace_old` is present. Tool count 19 → 18; the merged
  definition costs 316 tokens against 245 for the two it replaced, i.e. +71 tokens
  once, in exchange for removing a 33%-of-calls failure mode.
- [x] **Fixed a silent data-loss default while merging.** `replacement_lines` was
  read with `unwrap_or_default()`, so a call that simply omitted the field
  **deleted the range**. Omitting both content fields is now an error; only an
  explicit `""` deletes. A test was written first and confirmed to fail against
  the old code before the fix went in.
- [x] **`copy` and `move` were one tool wearing two names — now merged.** The old
  item deferred this on "only 23 calls, too little data to prove the shape is
  right". Both revisit triggers fired: usage rose, and a wrong-mode failure showed
  up — `[E_RANGE_ORDER] remove_from must come before remove_to: 232..=230` on a
  `move` call whose schema never had a `remove_from`. That message was the real
  defect: one error string served `replace`, `copy` and `move`, but only `replace`
  names its ends `remove_from`/`remove_to`, so it told models to fix an argument
  that did not exist. It now says "the first anchor must come before the second",
  which is true of all three.

  Merged into `copy` with a `remove_source` boolean, absent means false so a model
  that forgets the flag copies rather than deletes. Measured: 18 tools / 2486
  tokens → 17 tools / 2419, and `copy`+`move` at 122+125 became 180. The cost is
  smaller than the old item's 247 because the flag's description is prose the
  combined entry would carry anyway; the real win is one choice instead of two.

  Tests: `copy_without_remove_source_leaves_the_source_in_place` (fails if the
  default flips to true), `copy_with_remove_source_removes_the_source_range`, and
  the name-list assertion extended to `move` being absent. The prompt audit caught
  both stale references — the system prompt's tool list and `anchor_grep`'s
  description — which is exactly what it exists for.

  Follow-up, not done: the `E_INVALID_RANGE` constraint applies only in move mode
  (the same anchors are legal for a copy), so the message now names `remove_source`
  and offers the copy fallback. Whether the flag should instead have been a
  separate `direction` enum is unknowable from 23 calls; the boolean is the smaller
  thing to reverse.
- [x] **`subagent` failed 64 of 88 calls (72.7%), investigated with
  `--example subagent_failures`.** The causes split cleanly, and only 24 are the
  tool surface's fault:

  | cause | n | fixable by |
  |---|---|---|
  | timed out | 28 | the caller (deadline) |
  | unknown model | 14 | surface |
  | provider http/401/429 | 12 | network/auth |
  | no credential | 5 | surface |
  | `unsupported tool functions.read` | 5 | surface |

  Fixed: the `functions.` prefix is now stripped from `tools` (5 failures were the
  OpenAI namespace leaking into the argument, and the whole call was lost over
  decoration); `unknown model` now ranks the configured models and names the
  closest ids instead of dead-ending (14 failures, all near misses — `gpt-5.2`,
  `gpt-5`, `sonnet`, `gpt-4o` — against a catalog the model had no way to read);
  `no credential` now says which provider lacks a key. `model` and `timeoutMs`
  gained descriptions. Cost: `subagent` went 111 → 202 tokens, +91 on every
  request. Judged worth it because each failed call burns a whole turn, but it is
  the most expensive tool definition after `bash` now.
- [x] **A tool call arrived as prose, and the "tool name" was an entire note.**
  Traced with `--example marker_leak`, which walks stored messages and separates the
  two cases that look identical in a traffic report. The `unknown tool: grep` entry
  was not a tool at all: a deepseek model emitted its native `<｜｜DSML｜｜ invoke
  name=` syntax inside the *content* stream, so parsing produced a call whose `name`
  field held ~350 characters of the model's own note plus the marker, and whose
  `arguments` held a real `bash` command. The call failed as an unknown tool, which
  means **the command never ran** — the failure was the safety net, not a breach.

  Root cause is not parseable away: `openai.rs` reads `function.name` correctly, and
  the model simply wrote a tool-call-shaped string into content. What was fixable is
  the reply. `ToolRegistry::unknown_tool_error` now bounds the echo to a leading run
  of identifier characters (so prose cannot come back), says when the name looks like
  prose rather than a name, lists the real tools, and suggests the nearest registered
  name when one is a substring — `unknown tool: grep` would have said `Did you mean
  `anchor_grep`?`. Both call sites (`execute_progress` and `ToolRegistry::run`) go
  through it so they cannot drift.

  A test caught a bug in the first attempt: clamping to 40 chars still reproduced
  `constraints> - 不做 defensive writing` verbatim, because the note's first sentence
  fits in 40 characters. Verified both new tests fail against the old message.

  Not fixed, deliberately: detection. Feather could watch text deltas for
  tool-call markers and either strip them or surface a notice. Left alone because a
  false positive would corrupt legitimate output that discusses this syntax, and the
  current behaviour is already safe — the call fails closed.

- [ ] **28 timeouts were self-inflicted, not the default.** Only 5 of 28 used the
  300000 ms default; the rest set 45–180 s themselves (9 at 120 s, 4 at 180 s, 2 at
  60 s). The schema now says real work needs minutes, but if timeouts persist the
  next step is a floor on `timeoutMs` rather than more prose.
- [ ] **A guessed model still costs a full round trip.** The ranking helps only
  after the fact. If it keeps happening, the schema could make `model` an `enum` of
  authenticated ids, built per request — at the cost of coupling the schema to
  local credentials.
- [x] **`not a live anchor` had a bug behind it, not just a cost.** The message
  blamed the model, but the anchor tools carry no `path`: `resolve_anchor` — how an
  edit finds its file — searched only the *resident* map cache (capped at 64
  files). Once a file was evicted, an anchor that still matched its line on disk
  resolved to nothing and the edit was refused. Reproduced in a test: read a file,
  map 72 other files, and the first one's anchor resolves to `[]`. Fixed with an
  eviction-proof anchor → path index (`ANCHOR_INDEX_CAPACITY` 65 536, cleared
  wholesale when full so it can never leak), refreshed on build and after every
  edit because edits re-draw anchors. Measured: `anchors.read` on 1500 lines stayed
  at baseline after hoisting the normalisation out of the per-line loop (calling
  `norm_key` per line cost ~3%).
- [x] **Two error messages were wrong, one embarrassingly.** `bad_anchor` passed the
  literal string `"op"` as the operation name, so copy/move failures read
  `op: XXXX is not a live anchor`. Both messages now name the tool and say what
  actually happened: anchors are re-drawn when a line is edited, so an anchor from
  an earlier read may be gone — re-read and retry. The old text ("read the file
  first") was wrong for the eviction case, where reading was never the problem.
- [ ] **Stale anchors are only partly explained.** Of 148 real failures, 11%
  happened on the *first* tool call after a read, which eviction alone cannot
  account for; 41% came 3-10 calls after a read. Re-run `--example
  anchor_staleness` once sessions accumulate against the fix to see how much of the
  rate was the cache bug.
- [ ] **`bash` failures are mostly policy, not capability.** 638 failures, of which
  the largest single group is `[ILLEGAL_COMMAND]` for redirection and in-place
  edits. That is the shell/anchor split working as designed, but a model that keeps
  trying is losing turns to it; worth checking whether the refusal text teaches the
  alternative strongly enough.
- [x] **The refusal text was teaching the wrong thing, and one guard was simply
  wrong.** Two separate defects, both from reading the code after the traffic
  numbers:

  The `>`-redirect check ran on a lexer that had already stripped quotes, so a
  quoted `>` became a bare `>` token and was refused as "shell redirection would
  write a file". `grep '>' notes.txt`, `awk 'BEGIN{print "a>b"}'`, and any patch
  body containing `->` were all blocked for writing files they never touch. The
  lexer now carries a per-token "came from quotes" flag (`lex_quoted`) and the
  redirect scan only fires on unquoted operators. The unquoted forms are still
  refused, and `/dev/null`, `NUL`, `2>&1` still pass — all three locked by tests.

  Every refusal also said "use the anchor file tools", which is internal jargon:
  the tools are named `write`/`replace`/`insert`. `illegal_with` now requires a
  concrete next step per case ("write it with `write` (path + content), or edit it
  with `replace`") and states what bash *does* allow.
- [ ] **The guard still lets real writers through.** `tee`, `cp`, `mv`, `dd`,
  `truncate`, `rm`, `sed -n 'w out'`, `awk -i inplace`, and `echo hi >&out.txt` are
  all unblocked, so the file-write protection is a speed bump, not a boundary.
  Also `python -c"..."` (attached, no space) escapes the inline-script check, and
  backtick/`$()` wrapping escapes the self-kill guard. Not fixed here: it is a
  sandbox problem, not a message problem, and closing it properly means deciding
  whether the bash tool is trusted at all.
- [x] **`web_fetch` and `web_search` each failed ~18%.** `web_fetch` discarded the
  size it had in hand and returned the same opaque "exceeds 2 MiB" from both the
  Content-Length and streaming paths; the messages now carry the measured size and
  say the HTML is not truncated to fit. `web_search` had a silent-success path:
  `parse_ddg` returned `Ok("No results found.")` for any page it could not parse,
  including challenge pages the three-substring check missed. It now requires a
  results container, so "not a results page" is an error while a genuine empty
  result set stays a success. Test proves the old behaviour returned
  `Ok("No results found.")` for an unrecognised page.

- [x] **Image input works, but only from the CLI.** Measured rather than assumed:
  `opencode-go/deepseek-v4.1-flash` declares `"input": ["text", "image"]` in the
  catalog and really does read images. `ui_vision` on a logo returned concrete
  detail (top-right quill dot, lower-right barb, mixed stroke terminals) that can
  only come from seeing the pixels, and `feather -m opencode-go/deepseek-v4.1-flash
  "…@crates/feather-gui/assets/feather-logo.png"` described it correctly. All four
  providers encode images natively (Anthropic `source`, OpenAI `image_url` data
  URL, Responses `input_image`, pi blocks); `tests/multimodal.rs` already covers
  ordering and the image-only case.
- [x] **Nothing checked the declared modality on the send path.** `models[].input`
  was read only by `ui_vision`'s own preflight, so attaching an image to a
  text-only model sent it anyway and the user got an opaque HTTP 400 mid-run.
  `models::accepts_images` now reads the field and `rpc::check_image_support`
  refuses before the image reaches the session, naming the model. Unknown models
  are not refused — a custom endpoint may accept images and the catalog has no
  opinion. Reverting the check reproduces `unwrap_err() on an Ok value`.
- [x] **The GUI can now paste and show images.** `alt+v` reads the Win32
  clipboard (`CF_DIB`, 24- or 32-bit, top-down or bottom-up), re-encodes to PNG
  once, and stages a removable chip; an image with no caption is a sendable
  prompt. `rows_from_messages` now gates on attachments as well as text, so an
  image-only turn survives a reload instead of disappearing. Measured cost for a
  1 MP attachment: `image.base64` 196 µs, `image.decode` 1154 µs, `image.encode`
  113 µs.
- [ ] **Drag-and-drop is still missing.** Pasting works, but dropping a file onto
  the window does nothing. `egui` reports dropped files in
  `ctx.input(|i| i.raw.dropped_files)`, so the work is to read the path, reuse
  `clipboard_image`'s decode, and stage it the same way `paste_image` does.
- [ ] **A quad-at-a-time base64 decoder was measured and is slower.** The
  obvious optimisation (index a `[u8; 4]`, one exact 3-byte store, no
  per-character branch) ran 196 µs -> 435 µs, stable across six runs: the
  bounds-checked writes cost more than the accumulator they replaced. Reverted,
  and both the shipped function and the bench copy say so with the numbers. Do
  not redo it without re-running `image.base64`.
- [x] **A second dead binding, same shape as the first.** Worth noting how it was
  found: grepping for the action name across the whole crate returned exactly one
  hit — the binding table itself. Any action with no consumer outside
  `keybindings.rs` is unbound in practice, and that grep is the cheap way to find
  the rest (`app.editor.external`, `app.suspend`, `app.session.fork`,
  `app.session.tree`, `app.session.togglePath` all look the same).

## Research note — persistent memory (2026-11-24)

Question: does persistent memory help, or does it record wrong judgments and
end up worse than not remembering?

**What the evidence says.** The failure mode is real and is the one you
described, but it is a property of *what* is stored, not of persistence itself.

- `LEDGER` (arXiv 2611.20927) is the most directly relevant result. It splits
  memory into a fact layer with **explicit rules for which value is current,
  which source is trusted, and what to delete**, plus raw-turn search as a
  fallback. On a coding-agent workload it answers **94.3% / 91.1%** (dev /
  held-out seeds) against **82.1% / 83.3% for full-history replay** — the
  strongest baseline. On LongMemEval it matches full replay (74.8%) using **1%
  of the context**. Three things follow: memory can beat full context; the gain
  comes from staleness rules, not from storage; and deletion has to be a
  first-class operation.
- The same paper shows what happens without those rules: facts round-tripped
  through a model are only as good as the model's extraction, and a memory store
  that never expires a value keeps serving it after it stops being true. Your
  concern is the failure mode of *inferred* memory, and it is documented.
- `MemoSync` (arXiv 2611.24165, N=132) tested the opposite of autonomy: the
  agent *proposes* memory candidates and the developer reviews, edits and scopes
  them, with the agent reporting which items it actually used. It beat two
  baselines, and a mediation analysis attributed the win to **monitoring
  accuracy** rather than to more memory. A memory the user cannot see or correct
  is the case that degrades.
- `Coding Agent Memory Post-Training II` (arXiv 2611.14009) finds the strongest
  memory behaviour is **file-based, using ordinary `bash` and a persistent
  workspace** — no special memory subsystem. The policy learns to write down,
  search and re-read files. Feather's `write`/`read`/`anchor_grep` already give
  that surface.
- `Security Impact of Coding Agent's Memory Files` (arXiv 2610.38521) is a
  caution on the same axis: malicious content in a loaded memory file changed
  secure-completion rates by **-8 to +6 points**, with no consistent pattern, and
  flagging the file as an attack did not reliably prevent vulnerable output. Any
  auto-loaded memory is an attack surface, which argues for memory the user
  authors or approves.
- `napkin` (a widely-copied Claude Code skill) is the popular form of the risky
  pattern: the agent continuously appends "mistakes, corrections, preferences"
  to a repo markdown file it reads at session start. It reports a behaviour shift
  by session 3-5, but nothing expires, nothing is verified, and the file only
  grows — exactly the accumulating-error failure mode.

**Assessment for Feather.** Do not build inferred-judgment memory. The evidence
supports memory that is (a) verifiable, (b) user-visible and editable, and
(c) expirable. Feather's existing `AGENTS.md` load (`config::project_context`,
global + per-directory, user-authored) is already the *safe* variant: the user
writes it, it is version-controlled, it can be deleted, and it holds durable
preferences rather than model guesses. Adding a model-maintained memory file
next to it would import the entire risk for a benefit that the four citations
above attribute to *rules and review*, which `AGENTS.md` already has in the
form the user controls.

If something more is wanted later, extend the safe direction first: search the
raw audit (`search_context`, above) beats storing a model's paraphrase of it,
because the raw audit cannot be wrong — it is what actually happened.

## RSI rounds — measured optimisation (2026-11-25)

Instruction: scan the codebase, improve compute and memory, and stop when three
consecutive rounds find nothing significant. Every entry below has a before and
after number; anything that did not move was reverted rather than kept.

### Round 1 — the turn boundary, 28.66 ms → 6.04 ms

`crates/feather-bench` baseline put `session.commit` at **28,656,400 ns/op** with
a 21,712 KiB peak. Three causes, each found by staging the call with
`crates/feather/examples/commit_stages.rs` rather than by reading the code:

- **A fresh `FileSnapshots` per turn.** Built inside `commit_turn`, so every turn
  got its own `OnceLock` and reopened `checkpoints.redb` from cold: 10.06 ms cold
  against 0.19 ms warm, confirmed with an `FEATHER_TRACE_OPEN` probe that logged
  two cold opens followed by nothing but cache hits. The handle now lives on
  `Session.file_snapshots`.
- **A metadata write that stored nothing.** `FileSnapshots::commit` did a full
  redb fsync to bump `meta.turn`, which is only read by the version `retain` loop
  — and that loop has nothing to retain when `meta.paths` is empty. Returning
  early took 10.05 → 8.08 ms.
- **A duplicate audit touch.** `flush_audit` already commits
  `(modified_ms, first_prompt)` in the same transaction as the turn's rows, and
  `commit_turn` wrote them a second time. `Session.touched_this_turn` makes the
  second write conditional: one fsync per turn instead of two, 8.08 → 6.04 ms.

`tokenizer.message` 148.8 → 127 ns and `cache.plan` 140,100 → 133,200 came along
for free. Peak RSS 21,712 → 10,728 KiB.

One test was deliberately strengthened: the reopen test asserted
`turns.len() <= 45` and **passed with persistence removed**. Tightening it to
`<= 12` made it fail as it should.

### Round 2 — the directory walk, `grep.anchor` 9.02 ms → 6.08 ms

`grep::collect_files` classified every entry with `Path::is_dir()`, which follows
links and always stats, and it allocated a `String` for every file name whether
or not a glob was set. `DirEntry::file_type()` answers from the directory entry
on Windows and from `d_type` on Linux, and the skip test can read the name as a
`Cow<str>`.

Guarded by `walk_descends_into_subdirectories_but_skips_ignored_and_hidden_ones`,
which asserts the walk still recurses into a real subdirectory and still refuses
`.hidden` and `target`. Verified failing first: forcing `is_dir = false` reports
`the walk must recurse into a real subdirectory; got []`.

### Round 2 — four things that measured flat, and were reverted

Recording these matters as much as the wins, because each looked obviously right:

- **`Durability::Eventual` on the per-turn snapshot.** A snapshot is a cache of
  the conversation and the audit trail is the truth, so skipping the fsync should
  be free. It changed nothing: `snapshot.full` 2,018,200 → 2,003,300 ns. The
  2 ms is redb's own commit path, not the fsync.
- **The same 2 ms is structural.** An empty one-row redb commit (`redb.commit`)
  costs 1,966,500 ns against `snapshot.reuse`'s 1,981,300 ns for 409 messages.
  Commit cost does not scale with content, so the only lever is *how many
  commits happen*, not how fast one is.
- **Testing the whole document before the per-line prefilter.** Removing what
  looked like a redundant second scan made it *slower*: 6.08 → 6.17 ms. The regex
  engine scans contiguous bytes more cheaply than a `strip_suffix` per line.
- **Caching `FileSnapshots::tracked_paths`.** Warm redb reads measured **0.00 ms**
  through the per-instance handle; there was nothing to cache.
- **An incremental accumulator for `compaction::file_ops`.** It scans all messages
  every turn, but at 1601 messages with real tool calls it costs **131 µs**
  (`crates/feather/examples/file_ops_scale.rs`). O(n) with a tiny constant is not
  a problem worth code.

### Why the turn boundary stops here

A turn now performs exactly two durable commits: the audit flush and the message
snapshot. They live in different databases and each costs redb's ~2 ms floor, so
6.04 ms is two floors plus ~2 ms of real work. Going lower means removing a
commit, and both remaining ones are what makes a turn recoverable — the audit
trail is the record of what happened and the snapshot is what makes resuming a
96.9 MiB session cheap.

### The redb floor bounds the rest

Rounds 3 and 4 found no win, and the reason is now measurable rather than a
feeling. Every case that sits near 2 ms is the *same* 2 ms:

| case | ns/op | what it actually is |
|---|---|---|
| `redb.commit` (one small row) | 1,970,100 | a bare commit |
| `snapshot.reuse` (409 messages) | 1,981,300 | the same, content irrelevant |
| `snapshot.full` (400 serialised) | 2,007,300 | the same |
| `checkpoints.grow` | 1,977,500 | the same |
| `anchors.read` (1500 lines) | 1,973,400 | `build_map`'s trailing `persist` |
| `session.commit` | 6,050,900 | ~3 of these + real work |

Commit cost does not scale with content — a one-row commit and a 409-message
snapshot are within 0.6% of each other. So the only lever is *how many commits
happen*, which is why round 1's two removed writes were worth 22 ms and why every
micro-optimisation inside a commit measures flat.

Confirmed negative results, each reverted rather than kept:

- **`Arc` in the anchor index.** `index_map` inserts one entry per line, each
  cloning a `PathBuf` under the mutex — 1500 allocations for a 1500-line file.
  Sharing one `Arc` changed `anchors.read` from 1,973,400 to 1,969,800 ns: the
  1500 clones are noise beside the one commit at the end.
- **`text_cow` in `push`.** The instruction-prefix test called `Message::text()`
  twice, and `text()` concatenates every text block into a fresh `String`. At a
  realistic 64 KiB tool result the replacement measured 224.9 µs against 221.4 —
  slower, within noise. Reverted.

### The known cost this work did not remove

`Session::push` clones the whole message into `pending_audit`, and
`crates/feather/examples/push_cost.rs` puts that at a flat **3.5 µs/KiB** — 225 µs
for a 64 KiB tool result, once per message. It is one full copy of the body.

The obvious fix is for `pending_audit` to hold an index into `messages` instead.
That is **not safe today**: `set_messages_for_compaction` and `repair_context`
replace the whole list, and the existing test
`resume_compacted_context_pages_originals_without_a_summary_bubble` does exactly
the dangerous sequence — 250 pushes fill the buffer, the list is replaced with a
single summary, then `commit_turn` writes it. An index would silently record the
summary 250 times in the audit trail.

Making it safe means proving `pending_audit` is empty whenever the list is
replaced, or remapping the indices there. Until that invariant is established the
copy stays: 225 µs per message is cheaper than an audit trail that lies about what
happened.
