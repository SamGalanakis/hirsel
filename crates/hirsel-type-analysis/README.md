# hirsel-type-analysis

Stage-selected analyses over `hirsel-types` and declaration snapshots:

- `cleanup`: destruction summaries and specialization by owned actuals.
- `logical`: target-independent logical sizing and resolved structure.
- `parametric`: MODE-2 parametric uniqueness and supplying arguments.
- `representation`: successive nominal representation expansion and exact storage
  shape relationships. Resource failures propagate through the owning phase.
- `spelling`: source-shaped diagnostic type names and disambiguation.

Each module owns its public functions; consumers import them directly. The crate
never queries implementation bodies. Callbacks retain stage-owned declaration
lookup, authority evidence and operational failures. Unknown declarations or
parametric content keep their documented unknown relation. Input size and depth
never substitute an unknown answer. Logical traversals use heap worklists; the
compiler resource guard reports actual allocation exhaustion.

Stable semantic definitions, graph transport and basic property queries stay in
`hirsel-types`. Canonical CT spelling stays there because it is used in instance
keys and compile-time evaluation. Nominal proof memoization and cleanup-bound
normalization stay beside the basic property queries that use them.

The extraction preserves algorithms and moves the existing tests with them.
