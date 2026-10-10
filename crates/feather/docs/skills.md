# Skills

Feather discovers Pi-compatible Agent Skills from:

- global: `$FEATHER_HOME/skills` (default `~/.feather/skills`)
- workspace: `<cwd>/.feather/skills`
- any configured `skills` paths in global/workspace settings

A skill is a Markdown file with YAML frontmatter. `SKILL.md` marks a skill root and stops recursion below that directory; root-level `*.md` files are also accepted when they have a description.

```md
---
name: refactor-helper
description: Guidance for safe Rust refactors
disable-model-invocation: false
---
Skill instructions here.
```

Names must be lowercase `a-z`, digits, and hyphens. `description` is required. Skills with `disable-model-invocation: true` are omitted from the system prompt list, but can still be invoked explicitly.

Slash forms:

- `/skill` returns a metadata-only listing for a chooser/list UI.
- `/skill NAME ARGS` loads `NAME` lazily and passes `ARGS` literally.
- `/skill:NAME ARGS` remains supported for compatibility.

Explicit invocation reads the file fresh each time and strips frontmatter from the body.