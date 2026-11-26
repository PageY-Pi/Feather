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
- [ ] **`copy` and `move` are one tool wearing two names.** Identical schemas
  (`source_from`/`source_to`/`insert_after`), and in 30 sessions they were called
  23 times combined against 4500 reads. Cost: 247 tokens on every request, forever,
  for a capability used once every few sessions. Not merged yet because the merge
  needs a destination-mode field and 23 calls is too little data to prove the shape
  is right; the risk of guessing outweighs 247 tokens for now. Revisit when usage
  rises or a wrong-mode failure appears in the traffic.
- [ ] **`subagent` failed 64 of 88 calls (72.7%).** Much worse than any hashline
  tool. Not investigated.
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
