# Web tools and GUI polish — 0.1.2

## GUI

Session/model/OAuth pickers and the slash-command menu now highlight on pointer
hover as well as keyboard navigation. Moving the pointer updates the selected
row; a stationary pointer does not override keyboard selection. Hover alone
never opens a session. Del still confirms deletion of the highlighted session.

The command menu's whole row is clickable, including its right-side description.
The native borderless window requests Windows 11 DWM rounded corners. egui
cards, menus and dialogs retain rounded frames on other platforms. DWM may
ignore the request on older Windows, remote sessions, fullscreen or maximized
windows. There is now only one maximize/restore button; fullscreen is **F11**.

## web_search / web_fetch

Previously Feather had neither tool. These are compiled Rust ports of the local
`pi-native-search` extension's routing/payload behavior. They are registered for
CLI/GUI agents and child agents, and require no Node/TypeScript extension runtime.

- `web_search {"query":"..."}`: official provider IDs `zai`, `anthropic`,
  `openai`, `xai`, `google` select native search: ZAI MCP Web Search Prime,
  Anthropic web_search_20250305, Responses web_search or Google grounding.
- `deepseek` (and any provider whose base URL is on `deepseek.com`) posts to the
  DeepSeek Search API, `POST https://api.deepseek.com/v1/tools/search`. One
  request returns ranked structured results instead of a model turn, so it works
  with every DeepSeek model rather than only the v4-pro server-side tool.
  Results render as index/title/URL/summary (summary, else snippet), are capped
  at eight, and URLs failing the fetch guard are dropped. Bases configured for
  another wire format (`/anthropic`, `/beta`) are folded back to the host root.
  Queries are cut to DeepSeek's 128-character limit. Billed per successful call
  (about $0.75 per 1,000) with a 5 QPS account limit.
- Unknown/custom provider IDs use DuckDuckGo HTML.
- Native failure is explicitly reported before DuckDuckGo fallback. Provider
  credentials are resolved at request time. Keys are never put in error messages
  or query URLs. Native search may incur provider charges/quota usage.
- Source URLs/citations are retained; results are marked untrusted web content.
- `web_fetch {"url":"https://..."}`: HTTP(S) public pages; HTML script/style
  content removed, whitespace normalized, entities decoded; JSON prettified.
  No JavaScript rendering, screenshots, authenticated site cookies or PDFs.
- Output limit: 50 KiB / 2000 lines; input limit: 2 MiB; overall timeout: 45s;
  cancellation stops outstanding requests. Both tools are parallel-safe.
- Fetch blocks local/private/reserved IP ranges and embedded URL credentials,
  validates every redirect, pins public DNS answers to prevent rebinding, and
  never forwards provider auth or uses system proxies for arbitrary URLs.
- DuckDuckGo is a fixed public domain, may use system proxies, and redirects
  only within duckduckgo.com over HTTPS. Anti-bot challenges are errors, not
  fabricated search results. Network availability/rate limits can still fail.

Difference from the Pi extension: the Claude Code JS agent-SDK bridge is not
embedded in this standalone Rust binary. Unsupported provider IDs get DuckDuckGo
instead. Google search adapter is included; this does not add a Google chat
provider. No dynamic plugins or private extension config is shipped.

MIT attribution is in `THIRD-PARTY-NOTICES.md`, also included by the installer.

Validation: local native HTTP request/citation fixtures, UTF-8 limits, HTML/DDG
parsing, cancellation and private target guards; a mocked DeepSeek Search request
checks the `/v1/tools/search` route, 128-character query cut, structured result
rendering, refused URLs, and sanitized 402/429 errors. Live public example.com
fetch and DuckDuckGo search both succeeded. No paid native requests were made. GUI
pointer-hover regression and workspace tests cover existing behavior.
