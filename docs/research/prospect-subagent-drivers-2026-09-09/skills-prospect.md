# Mini prospect: Pi-style filesystem skills

Reference: local clean clone `/tmp/ref-pi-skills`, verified HEAD `05c6229813414010445558db9a80c84e15d65e70`, origin `https://github.com/badlogic/pi-mono.git`. The pre-existing conflicted `/tmp/ref-pi-mono` was not used. Scope: three transferable mechanisms for the user's explicitly requested simple skills implementation. No tests were executed for this reader pass.

## Recommendation

Add filesystem `SKILL.md` discovery, a compact name/description/absolute-path catalog in Agent guidance, lazy body reading through existing `shell.run`, and `/skill:name args` expansion at the shared host submission boundary. Keep current in-tree plugin prompt packs intact. No package manager, plugin lifecycle, or new execution engine is needed.

The current Hirsel plugin mechanism is deliberately different: `crates/hirsel-host/src/plugins.rs:333-378` sorts plugin `.md` files and appends their full bodies, and `lib.rs:873-884` adds those sections to host-generated guidance. `docs/plugins.md:78-84` and ADR 0014 settle that behavior. A filesystem catalog is an additive capability, not grounds to replace plugin skills.

## 1. Progressive disclosure with explicit path context

Pi's catalog contains only name, description, and location. The guidance tells the model to load the file through `read`, or `bash` when no read tool exists, and resolve relative references against the skill directory. `disable-model-invocation: true` hides a skill from this catalog without removing explicit command access. XML metadata is escaped. [Catalog implementation](https://github.com/badlogic/pi-mono/blob/05c6229813414010445558db9a80c84e15d65e70/packages/coding-agent/src/core/skills.ts#L348-L392).

Adopt this directly, naming Hirsel's actual `shell.run` tool. Include an absolute `SKILL.md` path and a clear instruction to read it before following the skill. Resolve `scripts/`, `references/`, and `assets/` relative to its parent directory rather than the host process cwd. The catalog should remain host-generated so editing the Owner's prompt does not remove discovery instructions. Optional `disable-model-invocation` is cheap and preserves expected cross-harness behavior.

## 2. Discovery has a boundary and collisions have a winner

Pi stops descending once a directory contains `SKILL.md`; supporting Markdown beneath that directory is not independently registered. Missing descriptions prevent loading; most name/length problems produce diagnostics. It parses YAML frontmatter, including block scalars, rather than inventing a line parser. [Discovery boundary](https://github.com/badlogic/pi-mono/blob/05c6229813414010445558db9a80c84e15d65e70/packages/coding-agent/src/core/skills.ts#L194-L220), [metadata validation](https://github.com/badlogic/pi-mono/blob/05c6229813414010445558db9a80c84e15d65e70/packages/coding-agent/src/core/skills.ts#L278-L344), [frontmatter parser](https://github.com/badlogic/pi-mono/blob/05c6229813414010445558db9a80c84e15d65e70/packages/coding-agent/src/utils/frontmatter.ts#L1-L41).

Duplicate physical files are canonicalized and skipped; duplicate names retain the first winner and report both paths. **Precedence nuance:** calling the low-level loader with defaults loads user before project, but the actual application resource loader supplies preordered paths with defaults disabled. Its project-before-user behavior is explicitly covered by the issue 2781 regression. Do not infer effective product precedence from the low-level default branch alone. [Collision mechanism](https://github.com/badlogic/pi-mono/blob/05c6229813414010445558db9a80c84e15d65e70/packages/coding-agent/src/core/skills.ts#L416-L455), [application call](https://github.com/badlogic/pi-mono/blob/05c6229813414010445558db9a80c84e15d65e70/packages/coding-agent/src/core/resource-loader.ts#L672-L682), [project/user regression](https://github.com/badlogic/pi-mono/blob/05c6229813414010445558db9a80c84e15d65e70/packages/coding-agent/test/suite/regressions/2781-skill-collision-precedence.test.ts#L91-L122).

For Hirsel, document one small root list and explicit project-before-user ordering, with deterministic traversal, canonical file deduplication, and collision diagnostics. Start with `SKILL.md` directories; Pi's loose Markdown discovery, package resources, settings filters, ancestor trust machinery, and reload ecosystem are not required. If following directory symlinks, track visited canonical directories to avoid cycles. Tolerate absent optional roots, but report malformed declared skills. Reuse a YAML parser for quoted and multiline descriptions; ignore unrelated frontmatter fields.

## 3. Explicit invocation expands into the actual user message

Pi recognizes a leading `/skill:name`, finds the catalog entry, rereads the file at invocation time, strips frontmatter, wraps the body with name/location/base-directory information, then appends arguments. That expansion is applied both to ordinary prompting and queued steer/follow-up input. [Expansion](https://github.com/badlogic/pi-mono/blob/05c6229813414010445558db9a80c84e15d65e70/packages/coding-agent/src/core/agent-session.ts#L1357-L1413), [ordinary prompt ordering](https://github.com/badlogic/pi-mono/blob/05c6229813414010445558db9a80c84e15d65e70/packages/coding-agent/src/core/agent-session.ts#L1211-L1229).

The expanded text becomes the user message and is persisted by normal message handling. This records the instructions actually used instead of relying on a skill file to remain unchanged forever. [User message construction](https://github.com/badlogic/pi-mono/blob/05c6229813414010445558db9a80c84e15d65e70/packages/coding-agent/src/core/agent-session.ts#L1265-L1277), [message persistence](https://github.com/badlogic/pi-mono/blob/05c6229813414010445558db9a80c84e15d65e70/packages/coding-agent/src/core/agent-session.ts#L668-L685).

Pi passes unknown skills through as ordinary text; read failures emit an extension error then also pass through. Its documentation says args use a `User:` prefix, whereas this pinned implementation appends them directly. Follow inspected code for the mechanism; Hirsel can choose a clearer explicit command error rather than silently treating a misspelled command as successfully invoked.

Hirsel should expand at host submission so every client gets identical behavior. Both `submit_owner_turn` (`lib.rs:464-521`) and `submit_addressed_turn` (`thread_commands.rs:30-94`) currently persist `body` and then construct `OwnerTurn` from that stored body. Expand before the first persistence and preserve attachments, mentions, addressing, and send/steer mode. Do not defer expansion until model execution: queued or recovered requests must see the captured instructions.

Crucial Hirsel-specific invariant: a retry with the same `client_id` must reuse its already-persisted expanded body even if the skill file has since changed or disappeared. Avoid rereading/validating a skill before checking an existing accepted request. Do not expand generated lifecycle/action labels merely because they happen to look like commands. Escape wrapper metadata and leave user arguments as ordinary content, not shell code.

## Verification outcomes for the implementation

- Catalog includes metadata/path but excludes skill body; multiline YAML descriptions and hidden-from-model skills behave as documented.
- Nested skill assets are not additional skills; project/user collisions are deterministic; duplicate symlinks do not duplicate discovery or recurse forever.
- Explicit invocation preserves body, relative-reference base, arguments, attachments, and thread ownership; unknown/unreadable skill errors are visible.
- Both Owner and addressed Thread submissions work; persisted expanded instructions survive queued delivery and file edits; duplicate `client_id` retry returns the original accepted request.

These checks exercise the actual feature boundaries; a model/provider call is unnecessary for verifying the loader and host submission contract.
