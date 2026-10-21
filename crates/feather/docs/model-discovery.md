# Provider model discovery

The model picker is fed by a catalog that ships inside the binary
(`crates/feather/src/builtin-models.json`, compiled with `include_str!`). A
provider that launches a model after a release is therefore invisible until the
next build — which is what `deepseek-v4.1-flash-economy` did on 2026-10-20.

`crates/feather/src/model_refresh.rs` closes that gap for providers with a
model-list endpoint.

## What runs, and when

| Moment | Behaviour |
| --- | --- |
| GUI launch | `spawn_startup_refresh` runs on a worker thread; the first frame is never blocked |
| CLI / RPC launch | same background thread; a short-lived one-shot may exit before it finishes |
| Any launch within 12 h of a success | no request at all |
| Explicit refresh | `model_refresh::refresh_now(&home)` ignores the cache age |

After a successful refresh the resolved catalog is rebuilt (`invalidate_catalog`)
and the GUI receives `UiEvent::ModelsRefreshed`, which re-reads
`models::load()` so the open picker updates without a restart.

## Why the payload is filtered

`GET {base}/models` on the Ark Coding Plan base URL answers with the **whole
platform** catalog, not the subscription's subset: 137 ids, of which 83 are
`Shutdown` and 17 `Retiring`, and the rest include video, 3D, translation and
embedding models. Handing that straight to the picker would bury the plan's
models and offer entries that cannot be called.

Rules applied in `accept`:

1. `status` of `Shutdown` or `Retiring` is rejected.
2. `domain`, when present, must be in the endpoint's list (`LLM`).
3. `task_type`, when present and non-empty, must include `TextGeneration` or
   `VisualQuestionAnswering`. Absent means "unknown", not "no": a few callable
   entries omit it.
4. The id must fold onto a name the plan documents (`ARK_CODING_PLAN`), compared
   without dashes, dots or case, so the platform's `deepseek-v4-1-flash-economy-261017`
   matches the documented `deepseek-v4.1-flash-economy`. The **longest** matching
   name wins, so `deepseek-v4.1-flash` never absorbs the economy variant.
5. Dated builds of one family collapse to a single entry; the newest build (the
   one advertising the larger context window) wins.

## Merge rules

`models::load_for` unions discovered models **after** applying `deletedModels`
and **after** the bundled catalog, so:

* a bundled entry always wins a name collision — it carries hand-checked limits
  and capability flags;
* a stale deletion entry cannot hide a model the provider is serving;
* an endpoint that changes shape, returns an error, or is unreachable removes
  nothing.

The list endpoint under-reports capability. It reports `input_modalities:
["text"]` even for `deepseek-v4-1-flash`, which is multimodal. A discovered model
therefore adopts its bundled sibling's `input` when the endpoint does not report
`image` — family membership is decided by the same normalized-prefix rule.

Thinking levels are not taken from the endpoint: `apply_manual_effort` assigns
the Ark map (`volcengine_thinking_map`) to every `volcengine*` model, and Ark
accepts `thinking` + `reasoning_effort` on its OpenAI-compatible endpoint.

## Credentials and failure

`StoredCredentialResolver::for_home` resolves the credential against the home
being refreshed — the same store and token-refresh path a chat request uses. Do
not use `StoredCredentialResolver::new` here: it reads the process-default home
and would pair one store's catalog with another store's credential.

Discovery is best-effort by construction. A missing credential, non-2xx status,
unexpected JSON, empty result or timeout keeps the cached/bundled list and
writes nothing. The HTTP request is bounded at 20 s and happens while **no**
config lock is held; the lock is taken only for the short write, so a refresh
can never stall the UI thread behind a slow provider.

## Verifying a refresh

```bash
# what the picker will show, straight from the resolved catalog
cargo run --release -p feather --example vision_models
```

An isolated home makes no request and must not lose cached models:

```bash
cargo test -p feather --lib model_refresh
```

The tests cover the live payload shape (retired builds, media models, two builds
of one family), the longest-name folding rule, the newest-build rule, unknown
payloads and the stale-cache path.

## Adding a provider

Add an `Endpoint` to `ENDPOINTS` with its list path and platform domains, then
teach `family_of` nothing new — it works from the plan's documented names. Two
rules of thumb:

* Prefer a name list the provider publishes for the subscription over the
  platform's raw ids; the two differ (Ark documents `kimi-k3` but does not list
  it in `/models`).
* Never let discovery remove a model. Union, do not replace.
