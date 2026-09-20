# Personal Preferences

Follow [CONTRIBUTING.md](CONTRIBUTING.md) for Hirsel's way of working, including
GitHub issues as the task tracker.

## General preferences

- If asked to do too much work at once, stop and state that clearly.
- Model routing and codex mechanics live in this repo's AGENTS.md
  ("Agent orchestration", "Codex mechanics") — one source of truth per repo.

## Parallel work: kiln forks

Do your work in a kiln fork on its own branch — never touch the main checkout:

- `F=$(kiln fork hirsel <name>) && cd "$F" && . ./env.sh` — the fork starts warm from the golden and carries the shared-pool config.
- Per-fork Cargo targets are private; managed Cargo does not use sccache or NativeLink, so the `kiln` operations (`build`, `test`, `clippy`, `doc`, `run`) are the fast path and direct cargo is the cold path.
- Parallel agents each get their own fork so edits don't collide.
- `kiln rm hirsel <name>` when done, after preserving the work (branch/PR).
