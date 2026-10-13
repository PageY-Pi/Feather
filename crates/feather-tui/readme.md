# Retired terminal frontend

The `feather-tui` crate and terminal loop are retired. They are not workspace
members and have no callers/dependencies in production. `feather` without a
headless prompt launches `feather-gui`; interactive work is GUI-only.

RPC/one-shot bridge remains for subagent automation and integration tests.
All original terminal implementation files were removed from active source.
