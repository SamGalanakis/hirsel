# Origin flow storage

`Indexed` numbers complete keys independently inside V. A shallow radix prefix
holds the first 512 keys; the remaining keys use a separate sparse radix tail.
The prefix is a storage boundary, not a fact-domain bound. Both segments retain
the original combined capacity, share the same key numbering across snapshots,
and participate in equality, iteration and joins. Independently numbered maps
still join by key. A present empty value remains distinct from a missing key.

The small/large domain split follows rustc's
[`MixedBitSet`](https://github.com/rust-lang/rust/blob/19833af7224424e8b53e288097d56b6af43d5bcf/compiler/rustc_index/src/bit_set.rs#L1057).
Hirsel keeps a sparse tail alongside the prefix because origin keys can first
appear during flow; rustc selects one representation from a fixed domain size.
Hirsel retains its existing persistent radix rows rather than copying a flat
array at each branch. The 512-entry prefix spans three existing radix levels.
The boundary/snapshot controls adapt rustc's
[bitset tests](https://github.com/rust-lang/rust/blob/19833af7224424e8b53e288097d56b6af43d5bcf/compiler/rustc_index/src/bit_set/tests.rs#L237),
with additional absent/empty and independently numbered-map cases required by
origin semantics.

Overwriting a value clears its seven origin fact rows through equality-checked
replacement. Empty rows stay unmaterialized, and shared collections can be
discarded without first cloning their contents. Retained/environment rows still
update their reverse holder indexes; name bindings, locations and expiration
history remain separate facts. Explicit empty entries inside a part or proof
map are removed only when the entire value is overwritten.
The allocation issue follows Rust's
[`Rc::make_mut`](https://github.com/rust-lang/rust/blob/a7c37f9f0ee5e93396106e9de20f4a86b4154e63/library/alloc/src/rcs/rc.rs#L2223):
requesting mutable access to a shared row clones it before `clear` can run.
Replacement uses the existing `Rows::set` equality check and copy-on-write path;
it introduces no alternative origin-join policy.

These changes preserve the full solver, reachability and audit classes. No
inference facts or entry certificates determine acceptance. The counterexamples
in `flow_evidence_laws.rs` remain controls for those boundaries.

`HIRSEL_VALIDATE_PROFILE=1` reports complete definition validation and individual
body flow separately. `hirsel-validate-flow-profile` rows name the definition,
body ordinal, optional `@preserves_unique` proof parameter and elapsed
nanoseconds. Body-flow intervals include model construction, fixed-point search
and reporting replay; they exclude the final model destructor, the enclosing
definition's type checks and read finalization. Nested body invocations remain
separate rows and can overlap their parent's interval. The timer is
observational and never changes the validation policy.

Operand type transfers reuse successful interned `TypeId` answers inside one
validation and body. The key contains the place and use mode, the root's must
facts, sharing, current uniqueness and ownership conflict, and the queried
projection's crossing, unique-part and receipt-path facts. These are the live
inputs read by `transferred_type`, `operand_unique` and `global_operand`.
The place fixes the declared type, projection and receiving storage; signature
views, generic declarations and static content eligibility stay fixed for the
validation. An update to any keyed fact selects another answer. No entry state,
producer certificate or inference memo is reused. Other type stores bypass the
memo; tooling failures are not cached and still report at each use.

The lookup/compute/publish structure follows Rust
[`execution.rs:279-332`](https://github.com/rust-lang/rust/blob/a7c37f9f0ee5e93396106e9de20f4a86b4154e63/compiler/rustc_query_impl/src/execution.rs#L279).
Unlike rustc's session query cache, this memo is single-threaded and dies with
V's scratch store. Its key includes live operand facts instead of only a type
and typing environment. Answers use the existing type interner's full equality;
no structural equality is replaced by unchecked pointer identity. The existing
projected-transfer law checks the independent uniqueness, crossing and lifetime
tables while changing these facts, and bounds repeated unchanged transfers to
one computation.

Flow propagates an unrefined output by reference, and transfers its final edge
by ownership. Switch/match discrimination and loop expiration request a private
snapshot only when they modify the edge. Existing entries join the borrowed
facts in place; a new final entry owns the output. This follows rustc
[`framework/mod.rs:359-375`](https://github.com/rust-lang/rust/blob/a7c37f9f0ee5e93396106e9de20f4a86b4154e63/compiler/rustc_mir_dataflow/src/framework/mod.rs#L359)
and [`direction.rs:175-215`](https://github.com/rust-lang/rust/blob/a7c37f9f0ee5e93396106e9de20f4a86b4154e63/compiler/rustc_mir_dataflow/src/framework/direction.rs#L175).
Hirsel additionally moves outputs into newly reached entries, and retains its
separate executable/audit classes and origin-expiration effects.

Quiet flow keeps the returns computed on the last fixed-point visit of each
executable return block. Every changed entry schedules another visit. Capture
snapshots already accumulate their joins during those visits. Replaying the
same converged entries without reporting would only recompute these outputs.
Reporting still transfers every converged entry in arena order, as rustc's
[`ResultsVisitor`](https://github.com/rust-lang/rust/blob/a7c37f9f0ee5e93396106e9de20f4a86b4154e63/compiler/rustc_mir_dataflow/src/framework/visitor.rs#L30)
reconstructs intermediate states from block entries. No intermediate iteration
emits diagnostics. The loop-return law checks the independently known loss of
uniqueness after a copy and bounds quiet transfers to fixed-point visits.
