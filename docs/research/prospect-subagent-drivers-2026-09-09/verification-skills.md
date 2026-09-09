# Independent verification: Pi-style filesystem skills

Reviewed 2026-09-09 against `/workspace/code/hirsel-skills` baseline `160b6dedea265d9d8f9ad182fc4425f70fbd2475`. Reference independently checked at `/tmp/ref-pi-skills`, HEAD `05c6229813414010445558db9a80c84e15d65e70`. I did not author `skills-prospect.md` or the implementation. Initial review was read-only, without concurrent Cargo execution. The coordinator subsequently delegated final checks; observed execution evidence is recorded below.

## Verdict

Pi mechanism claims are substantiated. The implementation fits the requested small feature: filesystem discovery and explicit invocation, additive to existing plugin prompt packs. The initial durability gap V-S1 was corrected and independently rechecked in `/workspace/code/hirsel-skills`: both submission APIs now capture expanded input atomically. No unresolved high-impact correctness issue found in this scoped review. Workspace tests, Clippy and formatting pass.

## Final execution evidence

All commands target `/workspace/code/hirsel-skills`, not the independently changing main workspace. No paid or live provider tests were run; no frontend source changed.

- `cargo test --workspace`: coordinator-launched rerun observed through completion in `/tmp/hirsel-skills-tests.log`. **370 passed, 0 failed, 4 ignored** across unit/integration/doc suites; `hirsel-host` has **289 passed**. The four ignored cases are the two real Claude/Codex CLI smoke tests, the public n0 relay integration, and an example doctest. The new `generated_action_labels_are_not_skill_commands` regression passes after correcting its fixture to an option-list choice whose label actually becomes the generated body.
- `cargo fmt --all -- --check`: executed by this verifier, exit 0; log `/tmp/hirsel-skills-fmt.log`.
- `git diff --check`: executed by this verifier, exit 0.
- `cargo clippy -p hirsel-host --all-targets -- -D warnings`: executed by this verifier, final exit 0; log `/tmp/hirsel-skills-clippy.log`. Its first run found only a new `unwrap_or_else(Vec::new)` lint in `skills.rs:47`; this verifier replaced it with behavior-equivalent `unwrap_or_default()` and added an explicit `Vec<PathBuf>` annotation. Final Clippy compiles all host targets. Formatting and diff checks were repeated after this change and still exit 0. Workspace test results above precede only this mechanical lint correction; the passing suite was not needlessly rerun.

## V-S1: Owner compatibility persistence gap — resolved after review

At initial review, `lib.rs:475-499` expanded the skill, committed `append_owner_message`, then separately enqueued the expanded `OwnerTurn`. `storage/chat.rs:62-106` committed the visible `/skill:...` body and `client_messages` receipt without any captured request. `lash_runtime/thread_queue.rs:7-16` persisted the expanded request only later, after another awaited database operation.

A crash or cancellation after the message transaction but before `save_thread_request` leaves a duplicate receipt without the instructions or work request. On retry, `owner_input_body` sees the existing receipt and bypasses skill loading; `append_owner_message` returns `inserted=false`; the caller acknowledges the message without enqueueing it. The Owner compatibility test exercises a completed successful submission followed by removal/retry, so it does not cover this boundary.

This split write **predates** this feature. It is relevant here because the feature explicitly claims captured durable input for both APIs; the addressed Thread implementation already uses a transaction and handles this correctly.

Recommended correction was to persist the Owner compatibility message, receipt, attachments and full expanded request in the same transaction before enqueueing. Preserve its existing anchor, legacy ping mentions, send mode and generated action context. Keep visible text short if desired; the captured expanded payload is the correctness requirement.

**Follow-up verified:** `lib.rs:475-486` now constructs the captured request including mode, mentioned pings and task action, then calls `append_owner_request`. `storage/chat.rs:73-94` validates its textual body; `append_owner_record` inserts expanded request metadata alongside the message/receipt/attachments before its transaction commits (`:135-149`). A duplicate returns its existing message without replacing the payload. The enqueue-error branch now calls only transactional `delete_chat_message`; that same transaction removes the matching thread request, attachments, receipt and message (`storage/chat.rs:172-187`). A temporary redundant request-only deletion was identified and removed before this final recheck. The initial crash window and cleanup split no longer exist in the inspected tree.

## Verified implementation properties

- **Parsing:** `skills.rs` uses `serde_yaml_ng`, supports quoted/folded/literal YAML scalars through the parser, ignores unrelated frontmatter fields, strips BOM, accepts LF/CRLF delimiter lines, validates nonempty metadata/body and limits each file to 256 KiB. Missing/invalid declared skills log diagnostics and do not enter the catalog.
- **Progressive disclosure:** `guidance()` contains name, description and absolute location, escapes XML metacharacters, omits bodies, directs the Agent to `shell.run` and supplies relative-reference context. `PromptConfig::agent_guidance` appends this to existing host guidance; `apply_agent_prompt` is called before each runtime queue drain, so edits become visible on later turns.
- **Explicit invocation:** only a leading `/skill:` command expands (leading whitespace tolerated); names split at whitespace; arguments remain ordinary text; frontmatter is removed; the body and base directory are included. Ordinary text containing `/skill:` later remains unchanged. Hidden-from-model skills remain available for explicit invocation. Unknown commands fail before new message insertion.
- **Discovery:** roots are ordered explicit environment roots, data-dir skills, working-dir `.agents/skills`, HOME `.agents/skills`. Directory traversal sorts children, uses canonical visited directories to avoid symlink cycles, stops at a directory containing `SKILL.md`, and keeps the first name winner with a diagnostic. Recursive depth is limited to 16 edges beyond root (deeper calls return). Optional missing roots are tolerated.
- **Addressed persistence:** `submit_addressed_turn` builds the expanded request before `append_thread_owner_request`; `append_thread_owner_record` captures it atomically with original visible message, receipt, attachments and addressing. Its new optional body field must be a string. Runtime `save_thread_request` uses `ON CONFLICT DO NOTHING`, so enqueue cannot overwrite the already-captured body. Duplicate submissions bypass expansion and the transactional receipt check avoids overwriting it.
- **Generated actions:** both submission paths bypass expansion when their generated action context is present. Inspected regression uses a generated label `/skill:absent` and expects that literal body, rather than an attempted skill invocation.
- **Plugin preservation:** baseline diff does not alter `plugins.rs`, plugin loading, or plugin skill bodies. `lib.rs` still composes `plugin_host.skills_prompt()` into the host section and adds filesystem skills through `.with_skills(...)`.

## Reference claims checked against the actual Pi clone

| Claim in skills-prospect.md | Independent result |
| --- | --- |
| Catalog is metadata only; hidden skills excluded; relative paths resolved from skill dir; metadata XML escaped | Confirmed in `packages/coding-agent/src/core/skills.ts:348-392`. |
| A declared `SKILL.md` stops descending into its support tree | Confirmed at `skills.ts:194-220`, including return after attempted load. |
| YAML parser handles block scalars; missing descriptions reject while many name problems are warnings | Confirmed in `utils/frontmatter.ts:1-41` and `skills.ts:278-344`. Pi additionally falls back to parent directory when name is absent. |
| Canonical physical-file deduplication plus first-name-wins diagnostics | Confirmed at `skills.ts:416-455`; Pi canonicalizes the file itself. |
| Low-level default root order differs from effective application project/user precedence | Confirmed: defaults load user before project; `resource-loader.ts:672-682` disables defaults for preordered paths; `test/suite/regressions/2781-skill-collision-precedence.test.ts:91-105` explicitly expects project over user over package. |
| Explicit command loads current body and passes unknown/read-failed input through | Confirmed at `agent-session.ts:1357-1385`. Actual arguments append directly; no `User:` prefix. |
| Expansion also applies before queued steer/followup | Confirmed at `agent-session.ts:1387-1413` and ordinary prompt path `:1211-1229`. |
| Expanded text becomes user message and normal persistence stores it | Confirmed at `agent-session.ts:1265-1277` and event persistence at `:668-685`. This does not by itself prove Pi has Hirsel's exact acceptance/retry durability guarantee. |

## Limits and nonblocking differences

- Hirsel intentionally requires a valid explicit `name` and nonempty body and rejects names Pi would merely warn about or derive from the folder. Document this rather than claiming byte-for-byte Pi compatibility.
- Hirsel deduplicates canonical directories, not independently symlinked `SKILL.md` files. Name uniqueness still prevents duplicate catalog entries, but two file symlinks to the same physical skill can produce a collision warning instead of Pi's silent physical-file deduplication. This is not a blocker for basic discovery/invocation.
- Bounds are per-file size and directory depth, not total discovered skill count, total scan work, or aggregate catalog size. Do not describe the catalog as globally bounded.
- Absolute location metadata is escaped. The human-readable base-directory line is plain text and the body intentionally remains raw instructions; this is instructional wrapping, not a strict XML document.
- The tests inspected cover multiline description/catalog/body/arguments, hidden skills, precedence and edit reload, symlink cycles, invalid names, unknown commands, addressed captured payload/reopen/retry, completed Owner acceptance/retry, and generated actions. Per-file size boundary, multiple attached blobs and explicit nonempty mentions are not directly exercised by these newly added cases. Their existing data paths are preserved by inspection; no broad new test project is necessary.

The initial code review made no implementation changes. V-S1 was reported immediately, corrected by the implementation coordinator, and independently rechecked as described above. During later delegated final checks this verifier made only the mechanical Clippy correction described in execution evidence. `docs/skills.md` also accurately documents root precedence, canonical-directory deduplication, file/depth bounds, strict metadata, hidden explicit invocation and retained plugin behavior.
