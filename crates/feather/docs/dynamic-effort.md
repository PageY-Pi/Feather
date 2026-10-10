# Server-only thinking effort discovery

Feather discovers efforts using authenticated `GET <base>/models` for each
**saved** model, grouping requests by provider endpoint/protocol. The embedded
catalog is retained for routing/model names, but its effort presets are not used.

The `dynamic_effort_v2` migration removes saved `reasoning`, `thinkingLevels`,
`thinkingLevelMap`, and `supportsReasoningEffort` capability presets (including
nested overrides) from `config.redb`. Routing, model IDs, credentials, limits,
protocol formatting and the user's requested effort preferences are preserved.
Writes cannot reintroduce those presets; server results live separately in the
`capabilities` document. The migration invalidates the old cache once only.
Original `.json.migrated` files remain archival backups and are not used at runtime.

CLI startup and GUI startup discover all saved providers, not just the current
model and not every bundled model. Fetch errors are recorded for affected models
with `fetched=false` and an error; other providers continue. Successful discovery
records `fetched=true`, timestamp, source and advertised levels/map. Successfully
fetched unknown results are cached too, avoiding repeated requests. Refetch
clears a model's entry explicitly; endpoint/protocol changes use a different key.

OpenAI/Anthropic-compatible APIs **do not standardize supported-effort metadata**.
A successful model list without those fields is stored as **unknown**, not
inferred from model names or a boolean reasoning flag. The picker only offers
server-advertised levels. When unknown/pending/failed, it shows a single local
`off` control (no requested thinking) and an explanatory status. This is not a
claim that the endpoint supports an explicit `off` effort. Protocol adapters
retain their wire-format/budget logic; that is not used to invent picker levels.

To force discovery without creating an agent/session:

```
cargo run -p feather --example fetch_efforts -- --force
```

The tool prints only model IDs, fetched status, levels and sanitized errors;
it never prints credentials. Without `--force`, successful cache entries are reused.

Live refresh on this machine: all 4 saved models across 3 providers were fetched
successfully, but none of the endpoints advertised effort metadata. They were
stored as fetched/unknown. No billable completion probes were sent.
