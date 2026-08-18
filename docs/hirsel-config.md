# Hirsel host configuration

Runtime-tunable host settings live in `hirsel.toml`, normally under `HIRSEL_DATA_DIR`. Set `HIRSEL_CONFIG` to use another path. The file is safe for the Owner or an Agent to edit and is hot-reloaded without restarting the host. Deployment/bootstrap settings such as ports, tokens, provider, driver, and data directory remain environment variables.

For loopback development, `HIRSEL_DEBUG=1` accepts any non-empty Owner token
across WebSocket and authenticated HTTP routes. Debug mode forces the Host to
loopback even if `HIRSEL_LISTEN` names another interface. Production mode
remains strict and requires the exact `HIRSEL_TOKEN`.

The current Host starts only the global Task orchestrator. Legacy side-session
wire compatibility is disabled by default and must be opted into explicitly:

```bash
export HIRSEL_COMPAT_SIDE_SESSIONS=1
export HIRSEL_SIDECHAT_TTL_SECS=86400 # optional; parsed only when enabled
```

Opt-in retains the compatibility backend but still opens no side Lash session
and starts no TTL reaper until the first legacy `open_side_chat` frame arrives.
Sub-agents, Processes, monitors, generated Task actions, and the global Agent
do not depend on this flag.

The main Agent selection is:

```toml
[model]
id = "google/gemini-3.7-flash"
variant = "default"
```

The selectable models are scoped to `HIRSEL_PROVIDER`, so `[model]` is read
against the provider the Host booted with:

- `openrouter` (the dev/run default): `google/gemini-3.7-flash`, one variant
  `default` — Gemini reasons on its own schedule, so the Host selects no effort
  and leaves reasoning to the provider. Requires `OPENROUTER_API_KEY`.
- `codex`: `gpt-5.6-sol`, variants `low`, `medium`, `high`, `xhigh`, and `max`.
  Requires a Codex CLI login.
- `anthropic`: no runtime-selectable models; the model is pinned by
  `HIRSEL_MODEL` and requires `ANTHROPIC_API_KEY`.

An `id` the running provider does not offer (for example a `gpt-5.6-sol`
selection left behind by a Codex-mode run) is not an error: the Host logs a
warning and falls back to that provider's configured default model.

The Sub-agent catalog is exactly the delegation lanes, grouped by CLI
provider — one row per lane, one reasoning level per row. There is no per-task
effort tuning, so a spawn that omits its variant always gets the lane's level.
`enabled` is the model-wide switch; setting it to `false` takes that lane out
of service. Entries naming anything outside the catalog are logged and ignored.

- `codex` `gpt-5.6-sol` at `high` — workhorse: judgment-heavy implementation
  and review-expensive verification.
- `codex` `gpt-5.6-luna` at `max` — economy: mechanically verifiable work.
- `claude` `claude-opus-5` at `high` — workhorse: taste-critical work (UI, API
  shape, copy) and fresh review of a finished diff.

```toml
[subagent_models.codex."gpt-5.6-sol"]
enabled = true
enabled_variants = ["high"]

[subagent_models.codex."gpt-5.6-luna"]
enabled = true
enabled_variants = ["max"]

[subagent_models.claude.claude-opus-5]
enabled = true
enabled_variants = ["high"]
```

Generative-UI templates live in the templates directory; see `templates/CATALOG.md`.
