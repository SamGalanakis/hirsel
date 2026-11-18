# Socket ownership and Net rules

Shared pure rule intersection and run-owned socket records for the evaluator and
native runtime. Language handles are eight-byte keys. Splitting changes the
record's directional owner bits and creates no record or allocation. The final
owner closes its OwnedFd; a write-half release first performs SHUT_WR.

OS attempts use the native reactor. Test harnesses can also supply owned
descriptors directly; no program or CLI operation can do so.

`model::Model` supplies the deterministic network and monotonic clock used by
both engines. Its configuration bounds each direction's accepted, transit and
readable queue independently and varies legal send/receive prefixes. Partition
acts on one send direction; reset discards undelivered bytes and preserves
readable bytes. A write-half release enables EOF after preceding bytes drain.

The harness records every byte-prefix and fault choice by default. A long law
that does not consume that trace may set `record_decisions` to false; its prefix
and seed still select exactly the same choices. Replaying a decision
prefix uses the same stable model handles; the remaining choices use the seed.
`poll` advances idle time to the next scheduled delivery, fault or deadline.
`repair` stops faults and repairs directions; callers then drain or signal every
session, release their handles and check that no operation remains pending.
Choice-prefix bounds and baseline-only sweeps describe partial coverage.

Model keys use generation zero, disjoint from every native registry generation.
They never recycle. Configured handle exhaustion or exhausting the model's u32
identity space returns `Other(24)` before creating a record.
