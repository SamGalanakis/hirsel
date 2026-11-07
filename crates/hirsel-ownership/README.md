# hirsel-ownership

The flow-sensitive ownership and locality check. Only declared signatures are queried.
[hirsel-modes](../hirsel-modes/README.md) documents it with the authority check, under
"Ownership and locality".

Allowed dependencies: hirsel-span, hirsel-syntax, hirsel-types, hirsel-diag, hirsel-intern,
hirsel-modes.

Ownership flow stores evaluated expression temporaries in `flow::temporaries::Temporaries`.
The domain is this item's source expression arena. Missing rows mean absent
values; constant leaves stay in their existing separate table. State snapshots
share the existing `hirsel_types::rows::Rows` radix tree, and updates copy only
the changed path. Iteration visits present expressions in their original ordinal
order. Value joins still run the original operation on every incoming present
row, including its first-join witness bookkeeping. No row-equality shortcut is
used to change that operation.

The storage uses the repository's existing persistent rows and Rust's
[`Rc::make_mut`](https://github.com/rust-lang/rust/blob/a7c37f9f0ee5e93396106e9de20f4a86b4154e63/library/alloc/src/rcs/rc.rs#L2223)
copy-on-write rule. Unlike copying a whole retained BTreeMap per node, the
snapshot shares its rows and mutations retain the other predecessor's facts.
The ownership forwarding law checks that snapshots copy no temporary payload
references and preserve the distinct unique/shared predecessor values; the
loop ownership corpus includes a conditional move that becomes invalid on
feedback unless every continuing path reinitializes the owner.
