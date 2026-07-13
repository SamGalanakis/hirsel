# Hirsel host configuration

Runtime-tunable host settings live in `hirsel.toml`, normally under `HIRSEL_DATA_DIR`. Set `HIRSEL_CONFIG` to use another path. The file is safe for the Owner or an Agent to edit and is hot-reloaded without restarting the host. Deployment/bootstrap settings such as ports, tokens, provider, driver, and data directory remain environment variables.

The main Agent selection is:

```toml
[model]
id = "gpt-5.6-sol"
variant = "medium"
```

The available main model is `gpt-5.6-sol`; its variants are `low`, `medium`, `high`, `xhigh`, and `max`.

Sub-agent models are grouped by CLI provider. `enabled` controls whether a model may be spawned, and `default_variant` is used when a spawn omits its variant. Codex CLI offers `gpt-5.5` (default `high`). Claude Code CLI offers `claude-opus-4-8` (default `high`), `claude-sonnet-5` (default `medium`), and `claude-fable-5` (default `high`). Every model allows `low`, `medium`, and `high`.

```toml
[subagent_models.codex."gpt-5.5"]
enabled = true
default_variant = "high"

[subagent_models.claude.claude-opus-4-8]
enabled = true
default_variant = "high"

[subagent_models.claude.claude-sonnet-5]
enabled = true
default_variant = "medium"

[subagent_models.claude.claude-fable-5]
enabled = true
default_variant = "high"
```

Generative-UI templates live in the templates directory; see `templates/CATALOG.md`.
