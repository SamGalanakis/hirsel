# Current Thread protocol validation

Use an isolated temporary data directory and unoccupied loopback port. Keep real user hosts/data untouched. Build with the repository-configured Cargo target: `cargo build -p hirsel-host`. For deterministic testing set HIRSEL_AGENT=scripted, HIRSEL_DRIVER=fake, HIRSEL_IROH=0 and HIRSEL_DEBUG=1. Supply explicit HIRSEL_TOKEN, HIRSEL_DATA_DIR, HIRSEL_CONFIG, HIRSEL_TEMPLATES_DIR and HIRSEL_LISTEN. Do not run model-spending scenarios without separate authorization.

The supported wire contract is [app/PROTOCOL.md](../../app/PROTOCOL.md). Hello uses tagged auth and no global history cursor. HelloOk supplies history_id and Thread summaries; OpenThread supplies the addressed conversation, turns and factual activity. ThreadTurn owner_message_id/agent_message_id and ThreadActivity.turn_id determine execution ownership. Execution completion never implicitly settles a Thread.

Automated current gates:

- `cargo test --workspace`: current schema rejection/initialization/reopen, Thread lifecycle/revisions/actions, pagination/mentions/attachments, artifacts/publication receipts, status timing, authenticated WebSocket/iroh, native offline queue/reconnect/store-identity reset, model/provider/prompt edits, monitor and subagent lifecycle, recovery/cancellation, terminal-delivery retries, fork triage, and push attention episodes.
- `cargo clippy --workspace --all-targets -- -D warnings` and `cargo fmt --all -- --check`.
- `bash android/build-native.sh`, then Android Kotlin compilation with the repository Android environment. Native clients must be rebuilt for this protocol; no old payload aliases remain.
- Frontend tests and desktop/phone browser proof against the current candidate host. Test running turn N while N+1 is queued, current turn ownership after pagination, current scheduled_digest text, lifecycle/filter actions, views, explicit artifacts, and history A→B clearing pending transport requests with reachable plain-text draft recovery.

Only use the isolated fixture for mutating tests. Exercise CreateThread/SendThreadMessage/ThreadAction with concrete IDs and generated-action revisions. Confirm process completion and meaningful activity timestamps, attachment roundtrip, per-Thread artifact discovery, and tagged cancellation. Validate repeated same-name tool calls remain distinct. Push tests use a recording/barrier sender, not real devices.

Real-provider semantic checks (explicit opt-in): multi-turn recall before/after restart, continuation/compaction, subagent completion/abandonment, monitor/timer wake triage, cancellation and later queue recovery. These use the same current Thread protocol; they are not a compatibility suite and are not run by the deterministic gate.

Operator cutover is separate: freshly inventory active/queued work and leases; wait idle; stop only the exact authorized host PID gracefully; cold-backup full data, config, binary and static assets. Initialize a fresh current schema and copy unchanged valid current records with their full artifact/message/Thread/reference closure and sequence high-water marks. Omit obsolete tables/imported diagnostics. Preserve auth/provider/prompt/plugin/project files and Lash checkpoints unless candidate-on-copy proves a specific incompatibility. Verify integrity/FKs, row/content/file hashes, current deserializers and read-only WS health before rollout. Never infer deletion permission from a schema error or reset automatically.
