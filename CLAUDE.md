# Personal Preferences

## General preferences

- If asked to do too much work at once, stop and state that clearly.
- If computer use is helpful for completing or verifying work, shell out to gpt-5.5 with Codex for it.

## Picking the right models for workflows and subagents

Rankings, higher = better. Cost reflects what I actually pay (OpenAI is near-free for me due to a deal), not list price. Intelligence is how hard a problem you can hand the model unsupervised. Taste covers UI/UX, code quality, API design, and copy.

| model | cost | intelligence | taste |
| --- | --- | --- | --- |
| gpt-5.5 | 9 | 8 | 5 |
| sonnet-5 | 5 | 5 | 7 |
| opus-4.8 | 4 | 7 | 8 |
| fable-5 | 2 | 9 | 9 |

How to apply:

- These are defaults, not limits. You have standing permission to override them: if a cheaper model's output doesn't meet the bar, rerun or redo the work with a smarter model without asking. Judge the output, not the price tag. Escalating costs less than shipping mediocre work.
- Don't let cost prevent you from using the right model for the job. Instead, take advantage of cheaper options to get more information and try things before moving the work to a more expensive option.
- Bulk/mechanical work (clear-spec implementation, data analysis, migrations): gpt-5.5 - it's effectively free.
- Anything user-facing (UI, copy, API design) needs taste >= 7.
- Reviews of plans/implementations: fable-5 or opus-4.8, optionally gpt-5.5 as an extra independent perspective.
- Never use Haiku.
- Mechanics: gpt-5.5 is only reachable through the Codex CLI - `codex exec` / `codex review` (my ~/.codex/config.toml defaults to gpt-5.5). Use the codex-implementation, codex-review, and codex-computer-use skills; for work they don't cover (investigation, data analysis), run `codex exec -s read-only` directly with a self-contained prompt.
- Claude models (sonnet-5, opus-4.8, fable-5) run via the Agent/Workflow model parameter.

Using gpt-5.5 inside workflows and subagents (the model parameter only takes Claude models, so use a wrapper):

- Spawn a thin Claude wrapper agent with `model: 'sonnet', effort: 'low'` whose prompt instructs it to write a self-contained codex prompt, run `codex exec` via Bash, and return the report (use `schema` on the wrapper to get structured output back).
- Always label these agents with a `gpt-5.5:` prefix, e.g. `{label: 'gpt-5.5:review-auth'}` - the workflow UI shows the wrapper's Claude model, so the label is the only indication the real worker is gpt-5.5.
- Codex runs can exceed Bash's 10-minute timeout: pass an explicit timeout, or run in the background and poll for the report file.
- **Codex launch protocol (mandatory):**
  0. ROOT CAUSE of the once-frequent startup hangs (RCA 2026-07-07): codex spawns the config.toml MCP servers at startup; the figments-local MCP (kubectl/curl against a possibly-rebuilding dev stack) and `npm exec ...@latest` servers can block the handshake indefinitely (0 CPU forever). Harness tasks never need codex-side MCPs — ALWAYS launch with them disabled:
     `codex -c 'mcp_servers.figments.enabled=false' -c 'mcp_servers.playwright.enabled=false' -c 'mcp_servers.runpod.enabled=false' -c 'mcp_servers.openaiDeveloperDocs.enabled=false' -c 'mcp_servers.runpod-docs.enabled=false' -c 'mcp_servers.wandb.enabled=false' -c 'mcp_servers.linear.enabled=false' exec --dangerously-bypass-approvals-and-sandbox --cd <dir> '<prompt>'`
     (If a new MCP server is added to ~/.codex/config.toml, add its disable flag here.)
  1. Spec in a scratchpad FILE; the inline prompt is a single-quoted one-liner pointing at it (backticks/$ in double-quoted prompts get shell-mangled).
  2. Launch as its own background task — never chained after git or other commands.
  3. Immediately arm a Monitor watchdog: `pgrep -f "codex-linux-x64.*<spec-name>"`, compare cumulative CPU every ~3 min. On a frozen reading, discriminate before killing: `ss -tp | grep pid=<pid>` — established TCP connections + occasional CPU ticks = the model is generating (long thinks accrue no client CPU; do NOT kill). Kill-and-relaunch only when CPU is frozen AND there are no established connections AND no file writes — the true hang signature is 0 total CPU from launch.
  4. Task is DONE only on its completion notification — 0-byte output files and missing pgrep hits both lie.
  5. Multi-day codex processes from other sessions exist on this machine — never kill anything that isn't yours by spec-path match.
- Parallel gpt-5.5 implementation agents must use `isolation: 'worktree'` so codex edits don't collide in the shared checkout.
- Workflow token budgets only count Claude tokens; codex work is free and invisible to `budget.spent()`.

## Parallel work: worktrees

When I'm using the main checkout, do your work in a git worktree on its own branch — never touch the main checkout:

- `git worktree add /workspace/code/hirsel-<name> -b <branch>`
- Export `CARGO_TARGET_DIR=/workspace/.cargo-target-<name>` in EVERY shell/codex invocation for that worktree (the ~/.bashrc env var beats cargo config files; sccache still shares compilation, so it's only link cost).
- Parallel codex/implementation agents must each get their own worktree so edits don't collide.
- Merge back when a cycle is validated; delete the worktree when done.
