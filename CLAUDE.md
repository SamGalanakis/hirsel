# Personal Preferences

## General preferences

- If asked to do too much work at once, stop and state that clearly.
- Model routing and codex mechanics live in the global CLAUDE.md
  ("Delegation") — one source of truth, no per-repo copy.

## Parallel work: worktrees

When I'm using the main checkout, do your work in a git worktree on its own branch — never touch the main checkout:

- `git worktree add /workspace/code/hirsel-<name> -b <branch>`
- `CARGO_TARGET_DIR` is now automatic: the shell profile derives `/workspace/.cargo-target-<repo-dirname>` from the git toplevel on every shell init, and `codex-harness-run` derives it from `--cd` when `--target` is omitted. Do NOT re-type inline `CARGO_TARGET_DIR=...` prefixes; an explicit export still wins if a run genuinely needs a different dir (sccache still shares compilation, so per-worktree dirs only cost link time).
- Before fanning out parallel codex/cargo runs in a fresh worktree, run `cargo fetch` once first — it pre-warms the shared package cache from the lockfile so concurrent first builds don't stack up 30s package-cache lock waits.
- Parallel codex/implementation agents must each get their own worktree so edits don't collide.
- Merge back when a cycle is validated; delete the worktree when done.
