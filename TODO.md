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
