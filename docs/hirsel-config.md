# Hirsel host configuration

Runtime-tunable host settings live in `hirsel.toml`, normally under `HIRSEL_DATA_DIR`. Set `HIRSEL_CONFIG` to use another path. The file is safe for the Owner or an Agent to edit and is hot-reloaded without restarting the host. Deployment/bootstrap settings such as ports, tokens, driver, and data directory remain environment variables. `HIRSEL_PROVIDER` is one too: it decides which provider the Host boots its agent session on, while the roster below decides which providers exist and which one each agent is pointed at from the next start.

For loopback development, `HIRSEL_DEBUG=1` accepts any non-empty Owner token
across WebSocket and authenticated HTTP routes. Debug mode forces the Host to
loopback even if `HIRSEL_LISTEN` names another interface. Production mode
remains strict and requires the exact `HIRSEL_TOKEN`.

Failed WebSocket authentication is throttled by the connection's source IP
address, regardless of its ephemeral source port. Hirsel does not trust
`Forwarded` or `X-Forwarded-For` headers for this identity. Deployments behind
a shared proxy therefore share one WebSocket authentication-throttle history
unless a future trusted-proxy contract explicitly provides client identity.

The Host runs addressed Thread conversations plus current subagent, monitor and fork-triage resources. There are no side-session compatibility flags or Event/Ping APIs.

History lives in `hirsel.sqlite`. New stores use the complete current schema 5
layout. Startup accepts that exact layout or an empty store and refuses every
other layout before modification. Back up the data directory before replacing
an older store. Configuration in `hirsel.toml`, auth/identity, plugins and
project files remain independent. See `e2e/thread-protocol/runbook.md` for
current validation and operator retention requirements.

Schema 5 adds the append-only `thread_turn_events` timeline. The Host commits
each typed event before broadcasting it and `open_thread` replays events only
for turns represented by its bounded message page (plus bounded message-less
turns on the newest page). Existing schema 4 histories can be preserved by a
separately reviewed offline operator that adds this initially empty table and
advances `user_version`; old reasoning or tool payloads are never reconstructed.

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
with `google/gemini-3.7-flash` as its default model) — but only when the
environment gives it a reason to exist: `OPENROUTER_API_KEY` is set and
non-empty, or `HIRSEL_PROVIDER=openrouter` (that mode needs the row to exist
before a key is pasted into it). A Host booting on Codex with no OpenRouter key
gets no keyless instance it could never call. It also seeds `[model].provider`
from `HIRSEL_PROVIDER` and `[model].id` from `HIRSEL_MODEL` when those keys are
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

For the main Agent, `[model].provider` is what the Host boots on. At startup it
resolves the main-agent provider once: the stored instance when it is set and
can actually boot, the `HIRSEL_PROVIDER` default otherwise. An
`openai_compatible` instance boots on **its own** `base_url` and `api_key` — an
edit to either is picked up at the next start, and `OPENROUTER_API_KEY` is a
first-boot seed that is never consulted again.

A stored choice that cannot boot — no `api_key` stored, an id that is not in the
roster, `claude` (Sub-agents only, ADR-0015), or `codex` with no readable
`~/.codex/auth.json` — falls back to the environment default and says so: a
warning naming the instance and the reason, and a standing notice on the
Providers tab, e.g. `configured provider "acme" is unavailable at boot: no API
key is stored — running on Codex`. Nothing is probed over the network, and no
key material appears in either.

What the model choice looks like depends on the selected provider:

- **`codex`** — a curated registry the Host validates against. The main Agent
  gets `gpt-5.6-sol` with variants `low`, `medium`, `high`, `xhigh`, `max`, plus
  `gpt-6-astra` with those efforts and `ultra` (default `medium`). Sol remains
  the default model. The fork gets `gpt-5.6-luna` (default `max`) plus `gpt-5.6-sol` as a deliberate
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
- **Main Agent provider** — at the next Host start, and it really is picked up
  there: the Host resolves `[model].provider` at startup and builds the running
  session's provider handle from it, including that instance's stored base URL
  and key. The handle is built once and baked into the session, so there is no
  live swap; the choice is stored and reported immediately while the session
  keeps running on the provider it booted with, and Settings shows which one
  that is. If the stored choice cannot boot, the Host falls back to the
  environment default and posts the notice described above.
- **A booted instance's `base_url` or `api_key`** — at the next Host start, for
  the same reason. The stored values are what the Host builds the handle from.
- **Fork provider and model** — stored only. No fork runtime consumes them yet.

## Native Lash coding workers

`threads.delegate` exposes `agent: "lash"` when at least one stored OpenAI-compatible provider has a non-empty API key. This runs a dedicated in-process Lash standard session; it does not change the coordinator's provider or RLM mode. With no explicit worker provider, Hirsel selects the configured `openrouter` instance and defaults its model to `deepseek/deepseek-v4.1-flash`. A non-OpenRouter provider requires an explicit free-text model. The worker variant is `default`.

Acceptance stores provider route identity, model, variant, canonical cwd, and tool-profile version, never an API key. A later base-URL change or provider removal cannot retarget queued work and produces a clear failure. API-key rotation remains private credential indirection for the same accepted route.

The worker's complete callable surface is the four Hirsel-owned tools `read`, `edit`, `write`, and `exec_command`; the last name is bound to Lash's semantic `shell.exec` operation, but does not use Lash's coding-tool implementation. The coordinated Lash runtime/provider dependencies stay at revision `47e6e23764939c790961fbe2905ee08ff5373a95`. It has no delegation, Thread management, artifact publication, browser/web, background-process, or plugin tools. Its cwd is a default path base, not a filesystem sandbox. Truncated text reads return `next_offset` and `next_byte_offset`; pass both back to continue a long Unicode line without skipping content. A child Task retains this executor preference and conversation on follow-up; changing its accepted profile opens a distinct worker-session generation with a bounded Task-only handoff.

`exec_command` uses non-login `/bin/sh` and is currently available only on Linux hosts with `pidfd_open` and readable procfs process metadata. Hirsel preflights both capabilities before spawning and owns each one-shot process group through terminal group termination, a no-runnable-member barrier, direct-child reap, and output drain; cancellation, timeout, history reset, output-reader failure, and ordinary completion all wait for that cleanup. Same-group descendants cannot outlive the result, while a command that deliberately escapes its process group is outside this guarantee. Unsupported hosts reject the call before spawning a process.

Hirsel's verified `1,048,576`-token context, `384,000`-token output limit, and image-input metadata applies only when the captured base URL is exactly `https://openrouter.ai/api/v1` and the model is exactly `deepseek/deepseek-v4.1-flash`. Provider instance names are local labels and do not establish capabilities. Every other free-text model uses conservative metadata; Hirsel validates the identifier's shape, while the configured endpoint remains the authority on whether that model exists and what it supports.

## CLI Thread models

The Thread model catalog is separate from the roster above and grouped by CLI
provider. `enabled` is the model-wide switch; `enabled_variants` restricts the
efforts available to `threads.delegate`. Omitting an effort uses the model's
default if enabled, otherwise its first enabled effort. Omitting the model keeps
Codex on Sol and Claude on Opus. Entries outside the catalog are logged and ignored.

- `codex` `gpt-5.6-sol` at `high` — workhorse: judgment-heavy implementation
  and review-expensive verification.
- `codex` `gpt-5.6-luna` at `max` — economy: mechanically verifiable work.
- `claude` `claude-opus-5` at `high` — workhorse: taste-critical work (UI, API
  shape, copy) and fresh review of a finished diff.
- `codex` `gpt-6-astra` with `low`, `medium`, `high`, `xhigh`, `max`, `ultra`;
  default `medium`.
- `claude` `claude-fable-5-1` with `low`, `medium`, `high`, `xhigh`, `max`;
  default `high`.

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

[subagent_models.codex."gpt-6-astra"]
enabled = true
enabled_variants = ["low", "medium", "high", "xhigh", "max", "ultra"]

[subagent_models.claude.claude-fable-5-1]
enabled = true
enabled_variants = ["low", "medium", "high", "xhigh", "max"]
```

### Native worker

The in-process Lash coding worker (`agent = "lash"` in `threads.delegate`) is
not a CLI lane: it has no curated model list and no reasoning efforts, so it has
its own row. `enabled` is the master switch — while it is off, the `lash` branch
is absent from the delegation contract entirely. `model` overrides the model the
default route opens on; omit it for the shipped default
(`deepseek/deepseek-v4.1-flash`). A delegation that names its own `model` still
wins over both.

The worker runs on the provider instances in `[providers]` that have an API key.
A delegation that names no `provider_id` is routed through `openrouter`; any
other eligible instance has to be named explicitly, together with a model. With
no keyed instance configured the worker is unavailable and Settings says so.

```toml
[native_worker]
enabled = true
model = "deepseek/deepseek-v4.1-flash"
```

Generative-UI templates live in the templates directory; see `templates/CATALOG.md`.

## Filesystem skills

See [skills.md](skills.md) for `SKILL.md` discovery and `/skill:name` invocation.
`HIRSEL_SKILL_DIRS` adds skill roots in path-list order before the defaults;
the Host also scans its data directory's `skills`, its working directory's
`.agents/skills`, and `~/.agents/skills`.
