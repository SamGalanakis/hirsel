# Single host binary with sqlite persistence, no Restate

Hirsel is one Rust binary (the Hirsel Host) embedding lash-runtime with `lash-sqlite-store` and the `NativeEffectHost`, in one repo that also contains the PWA client and the agent-editable UI templates. We deliberately skip lash's first-party Restate adapter even though hirsel dogfoods lash: for a single-player system on one VM, Restate is an extra service to operate for replay guarantees we mostly don't need — sqlite per-turn commits give restart-survivability, and Sub-agent sessions are cattle re-spawned from task specs anyway. The lash `EffectHost` boundary keeps the door open to Restate later.

Lash is consumed at the immutable Git revision pinned in `Cargo.toml`, never as a path dependency to the development checkout. All Lash packages use the same revision. The current upstream package names use the `lash-internal-` prefix; Hirsel retains their Rust import names through Cargo dependency aliases. The regex engine is now the upstream `lash-regress` workspace package and needs no consumer patch.

Lash upgrades can require recreating its SQLite databases; see the [deployment upgrade notes](../../deploy/README.md#lash-storage-upgrades).

Known consequence (from the lash embedding investigation, 2026-07-08): with `NativeEffectHost`, in-flight Durable Waits (e.g. an RLM turn parked on a sub-agent completion) do not survive a host restart. This is acceptable by design — ADR-0004 makes restart recovery Agent judgment anyway: processes go Abandoned, the Agent wakes and re-decides. If crash-durable waits ever matter, lash's sqlite-backed effect host is the first stop, not Restate.
