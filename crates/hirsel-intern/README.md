# hirsel-intern

The shared hardware instruction counter and session anchor assignment.

`instructions::InstructionCounter` owns one pinned, per-thread Linux perf event.
The driver uses it for request cost and optional front-end observation. Reads
refuse multiplexed or unavailable counters.

`Scramble` maps dense anchor positions to nonzero session IDs. The `id-salt`
feature permutes assignment for identity-invariance tests. Hirsel-span uses this
assignment rule. The crate has no production dependencies.
