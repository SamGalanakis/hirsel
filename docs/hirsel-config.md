# Hirsel host configuration

Runtime-tunable host settings live in `hirsel.toml`, normally under `HIRSEL_DATA_DIR`. Set `HIRSEL_CONFIG` to use another path. The file is safe for the Owner or an Agent to edit and is hot-reloaded without restarting the host. Deployment/bootstrap settings such as ports, tokens, driver, and data directory remain environment variables. `HIRSEL_PROVIDER` is one too: it decides which provider the Host boots its agent session on, while the roster below decides which providers exist and which one each agent is pointed at from the next start.

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

## The provider roster

`[providers]` lists the provider instances the two resident agents — the main
Agent and the wake-triage fork — can run on.

Two instances are built in, synthesised by the Host rather than stored: `codex`
(the local Codex OAuth login at `~/.codex/auth.json`) and `claude` (the local
Claude CLI credentials). They hold no key in this file, they cannot be removed,
and Settings shows each one's detection status — which path was probed, whether
credentials were found, and a non-secret account hint. `claude` is available to
Sub-agents only and is never selectable as a resident agent's provider; ADR-0015
records why. Both ids are reserved.

Every other entry is an OpenAI-compatible endpoint:

```toml
[providers.openrouter]
kind = "openai_compatible"
label = "OpenRouter"
base_url = "https://openrouter.ai/api/v1"
api_key = "sk-or-v1-..."
default_model = "google/gemini-3.7-flash"
```

Instance ids are `[a-z0-9][a-z0-9_-]{0,31}`. `base_url` and `default_model` are
required; an entry missing one is logged and ignored rather than failing the
boot.

**API keys live in this file and nowhere else.** The wire never carries one: a
client is told only whether a key is set and its last four characters, and even
that tail is withheld for keys shorter than eight characters. Keys are never
logged — a warning names the instance id and the reason only.

### Seeding from the environment

The first time the Host writes a `[providers]` table for a config file, it seeds
one OpenAI-compatible instance (`openrouter`, pointed at OpenRouter's base URL
with `google/gemini-3.7-flash` as its default model), writing `api_key` only if
`OPENROUTER_API_KEY` is set and non-empty. It also seeds `[model].provider` from
`HIRSEL_PROVIDER` and `[model].id` from `HIRSEL_MODEL` when those keys are
absent.

The presence of the `[providers]` table is the once-only marker. A file that
already has one — even an empty one — is never re-seeded, so a later change to
`OPENROUTER_API_KEY` never overwrites a key stored here.

## Per-agent provider and model

Each resident agent picks a provider from the roster and a model from it:

```toml
[model]
provider = "codex"
id = "gpt-5.6-sol"
variant = "high"

[fork]
provider = "openrouter"
model = "google/gemini-3.7-flash"
variant = "default"
```

`provider` is optional. With no `provider` key the agent stays on whatever
`HIRSEL_PROVIDER` the Host booted with. A `provider` naming an instance that no
longer exists — or naming `claude` — is not a boot error either: the Host logs a
warning and falls back to the booted provider.

What the model choice looks like depends on the selected provider:

- **`codex`** — a curated registry the Host validates against. The main Agent
  gets `gpt-5.6-sol` with variants `low`, `medium`, `high`, `xhigh`, `max`; the
  fork gets `gpt-5.6-luna` (default `max`) plus `gpt-5.6-sol` as a deliberate
  escalation. An off-registry id or variant is a rejected command.
- **An OpenAI-compatible instance** — the model id is free text: whatever the
  endpoint offers. The Host validates only that it is non-empty and carries no
  leading or trailing whitespace, and sets `variant = "default"`, because
  reasoning effort is the endpoint's business.
- **`anthropic` boot mode** — a legacy path, not a roster instance. It has no
  runtime-selectable model (the model is pinned by `HIRSEL_MODEL`, and
  `ANTHROPIC_API_KEY` is required) and no fork configuration.

An `id` the selected provider does not offer (for example a `gpt-5.6-sol`
selection left behind by a Codex-mode run) is not an error: the Host logs a
warning and falls back to that provider's default model.

### When a change takes effect

- **Main Agent model** — from the Agent's next turn. The selection is validated,
  persisted, and applied to the live session before the op returns.
- **Main Agent provider** — at the next Host start. The provider handle is built
  once at boot and baked into the running session; there is no live swap, so the
  choice is stored and reported immediately while the session keeps running on
  the provider it booted with. Settings shows which one that is.
- **Fork provider and model** — stored only. No fork runtime consumes them yet.

## Sub-agent lanes

The Sub-agent catalog is a separate surface from the roster above: it is the
delegation lanes, grouped by CLI provider — one row per lane, one reasoning level per row. There is no per-task
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
