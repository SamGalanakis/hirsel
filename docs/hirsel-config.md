# Hirsel host configuration

Runtime-tunable host settings live in `hirsel.toml`, normally under `HIRSEL_DATA_DIR`. Set `HIRSEL_CONFIG` to use another path. The file is safe for the Owner or an Agent to edit and is hot-reloaded without restarting the host. Deployment/bootstrap settings such as ports, tokens, driver, and data directory remain environment variables. `HIRSEL_PROVIDER` is one too: it decides which provider the Host boots its agent session on, while the roster below decides which providers exist and which one each agent is pointed at from its next turn.

For loopback development, `HIRSEL_DEBUG=1` accepts any non-empty Owner token
across WebSocket and authenticated HTTP routes. Debug mode forces the Host to
loopback even if `HIRSEL_LISTEN` names another interface. Production mode
remains strict and requires the exact `HIRSEL_TOKEN`.

Failed WebSocket authentication is throttled by the connection's source IP
address, regardless of its ephemeral source port. Hirsel does not trust
`Forwarded` or `X-Forwarded-For` headers for this identity. Deployments behind
a shared proxy therefore share one WebSocket authentication-throttle history
unless a future trusted-proxy contract explicitly provides client identity.

The Host runs addressed Thread conversations plus current subagent, Lash process, trigger, and fork-triage resources. Native execution is one TypeScript RLM session with process and trigger abilities. Every Thread carries the same full tool set and may select Native, Claude CLI or Codex CLI; Space-chat and worker roles are prompt guidance, not execution profiles. Registered processes and subscriptions live in per-Thread Lash stores; Hirsel projects them into the scoped Processes view and turns wakes and terminal results into conversation messages. There are no side-session compatibility flags or Event/Ping APIs.

History lives in `hirsel.sqlite`. New stores use the complete current schema 17
layout. Startup accepts that exact layout or an empty store and refuses every
other layout before modification. Back up the data directory before replacing
an older store. Configuration in `hirsel.toml`, auth/identity, plugins and
workspace files remain independent. See `e2e/thread-protocol/runbook.md` for
current validation and operator retention requirements.

Schema 17 keeps Thread icons, execution preferences, process deliveries,
bounded message Task focus and durable effect receipts. `message_task_focus`
links one accepted Owner message to a reachable Task and stores an object
snapshot; `meta['project_chat:home_thread_id']` points to the idempotently
bootstrapped Home Space. `thread_effect_receipts` records the Thread, artifact
or root target touched by an accepted turn operation. Receipt identity is the
turn, operation and effect index; replay does not duplicate it. Refusal details
exist exactly on refused effects. Current actions such as Stop or Archive are
derived projections and are not stored as claims in a receipt. Material-state
changes feed `thread_change_deliveries`; one cursor per Space chat advances
only after successful terminal consumption. `thread_turn_contexts` stores the
exact accepted conversation, focus, references and bounded change digest
before execution enqueue, so recovery never rebuilds a changed payload. These
delivery associations grant no reach, and admission rechecks current reach
before including content. The schema's
CHECKs link terminal states to completion timestamps and enforce absent starts
for queued work and actual starts for running work. Immutable `accepted_at`
records acceptance separately. Instruments use SQL NULL for absence; nonempty
validated component objects and arrays remain supported. Cancellation intent
lives on `thread_turns.cancel_requested_at`. Report receipts retain only the
activity reference; activity ids provide ordering and the activity holds the
report payload. Schema 8 also binds every push token to its authenticated device, restricts
platforms to supported values, and loads delivery targets only for unrevoked
devices. Each agent role keeps one typed SessionProfile JSON meta row.
There is no in-place migration: older schema versions and obsolete or branch-specific layouts
require backup and fresh-data handling before this build can start.

Schema 5 added the append-only `thread_turn_events` timeline. The Host commits
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
default_model = "deepseek/deepseek-v4.1-flash"
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
with `deepseek/deepseek-v4.1-flash` as its default model) — but only when the
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
model = "deepseek/deepseek-v4.1-flash"
variant = "default"
```

`provider` is optional. With no `provider` key the agent stays on whatever
`HIRSEL_PROVIDER` the Host booted with. A `provider` naming an instance that no
longer exists — or naming `claude` — is not a boot error either: the Host logs a
warning and falls back to the booted provider.

For the main Agent, `[model].provider` is what every Thread runs on. At startup
the Host resolves it once for the session it opens: the stored instance when it
is set and can actually boot, the `HIRSEL_PROVIDER` default otherwise. It is not
frozen there — a later change to `[model].provider`, from Settings or from this
file, repoints the default route and every Thread session is rebound to it
before its next turn. An `openai_compatible` instance runs on **its own**
`base_url` and `api_key` — an edit to either is picked up when the Thread is next
bound, and `OPENROUTER_API_KEY` is a first-boot seed that is never consulted
again.

A provider the Host cannot build a transport for — no `api_key` stored, an id
that is not in the roster — is not a route: the choice stays stored and
reported, a warning names it, and Threads keep running on the booted provider.

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

## Per-Thread Native routes

Settings names the Native provider and model every Thread runs on by default. One Thread can run on another: the Owner picks a provider and model in Thread Info's "Runs on" row, and the Agent names the same thing as `threads.delegate` with `agent: "native"` plus an optional `provider_id` and `model`. Both are validated against the same provider roster the Settings picker offers — the instance must exist and be agent-selectable (`claude` is Sub-agents only, ADR-0015), and the model must be one the instance offers, or free text where the instance takes free text. Native takes no reasoning variant: the model's own default effort applies.

A delegation that names neither `provider_id` nor `model` runs where its parent runs; the Settings default applies only when the parent has no Native route of its own.

The choice applies from the Thread's next turn. Its resident session opens on the booted provider and is rebound to the Thread's own when the next turn is admitted, so a turn already running keeps the backend it started on. Clearing the choice puts the Thread back on the Settings default the same way.

## The four coding operations

A Native session advertises four Hirsel-owned coding tools — `read`, `edit`, `write` and `exec_command` — beside the ordinary Thread tools. The last name is bound to Lash's semantic `shell.exec` operation, but does not use Lash's coding-tool implementation. The coordinated Lash runtime/provider dependencies stay at revision `47e6e23764939c790961fbe2905ee08ff5373a95`.

Acceptance stores the provider id, model and canonical cwd, never an API key. `cwd` is the directory those coding operations are rooted at — execution context, not a filesystem sandbox — so it is captured with the turn but never part of the public target. A later base-URL change or provider removal cannot retarget queued work and produces a clear failure. API-key rotation remains private credential indirection for the same accepted route.

Truncated text reads return `next_offset` and `next_byte_offset`; pass both back to continue a long Unicode line without skipping content. A child Task retains its executor preference and conversation on follow-up; changing the advertised tool surface opens a distinct session generation with a bounded Task-only handoff.

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

## Filesystem skills

See [skills.md](skills.md) for `SKILL.md` discovery and `/skill:name` invocation.
`HIRSEL_SKILL_DIRS` adds skill roots in path-list order before the defaults;
the Host also scans its data directory's `skills`, its working directory's
`.agents/skills`, and `~/.agents/skills`.
