# Configuration, custom APIs and effort discovery

Feather now stores global settings, custom providers/models, deleted model IDs,
and per-model capability discovery in `~/.feather/config.redb` (`FEATHER_HOME`
overrides this directory). Transactions preserve unknown settings. A separate
`config.lock` serializes short-lived database opens across the CLI and GUI.

On first use, `settings.json` and `models.json` are imported together in a single
transaction. After commit, they are renamed to `.json.migrated` backups. The
migration marker prevents re-import even if a backup rename fails. Do not edit
those backups to change current configuration. They contain the original API
keys; protect them as you would the database. redb is persistence, not encryption.
Project-local `.feather/settings.json` remains supported for scoped overrides.
OAuth credentials remain in the existing auth store.

## GUI

The top-right **+add** button opens:

- **Compatible API / models**: choose OpenAI or Anthropic compatible, enter a
  provider ID, base URL (including `/v1` when required), API key or `$ENV`, exact
  model ID, optional display name and token limits. Save persists the model and
  starts capability discovery. Reusing a provider ID shares its URL/key among
  models, so changing those fields affects all models under that provider.
- **OAuth / built-in providers**: the existing browser sign-in/key management.

Configured models have Edit, Delete (with confirmation), and Fetch/Refetch.
Deleting a model does not delete sessions. An active model deletion chooses
another available catalog model. Editing preserves advanced compatibility and
multimodal metadata.

The session chooser offers Delete beside each history row and asks for
confirmation. An open/locked session must be left before it can be deleted.
Deletion removes the session directory and its catalog entry; arbitrary paths
and symlink targets are rejected.

## Thinking effort

Discovery uses authenticated `GET <base URL>/models`, with OpenAI bearer auth or
Anthropic `x-api-key` and `anthropic-version` headers. It runs asynchronously in
the GUI, and for the selected provider at CLI startup. It does not generate
billable chat requests to guess supported levels.

Compatible APIs have **no universal supported-effort metadata field**. Feather
recognizes explicit lists such as `thinkingLevels`, `thinking_levels`,
`supported_thinking_levels`, `supported_reasoning_efforts`, `reasoning_efforts`,
`capabilities.reasoning_effort`, `capabilities.thinking_levels`,
`reasoning.effort.enum`, and explicit `thinkingLevelMap` metadata.

Successful discovery is cached with `fetched: true`, timestamp and source for
**each configured model**. Explicit levels override local picker and provider
maps. A successful response without metadata (or without the requested model)
is cached as **fetched / unknown**, and local catalog fallback is retained.
Unknown is not presented as server-confirmed support. Errors are visible and
retryable, not falsely marked fetched. Refetch explicitly clears that model's
cache; endpoint/protocol/model changes naturally use a new cache identity.
Existing successful fetches cause no repeated network discovery on restart.

Tests use local HTTP fixtures and temporary databases, never real API keys.
