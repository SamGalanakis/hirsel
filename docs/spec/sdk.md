# Generated TypeScript bindings

`hirsel sdk ts <program> --out <dir>` builds the program and runs its describe
launch mode. The generated ES modules and `.d.ts` declarations use the same
code in Node and browsers, without a build step.
The output directory has `package.json` with `"type": "module"`, so Node loads
the same `.js` files even inside a CommonJS project.

## 4. Generated code

`codec.js` follows `std.codec` value schemas and ordered key schemas. Integers
through 32 bits are `number`; u64 and i64 inputs and outputs are `bigint` only.
Bytes are `Uint8Array`, strings are strict UTF-8, records are objects in declared
field order, and tuples are TypeScript tuples. Maps retain insertion order.
Unit is `undefined`. Floats encode canonical NaNs and reject other NaN patterns
when decoding. Decoding rejects malformed tags and trailing bytes.

Unit variants are `{tag}`. One-field variants are `{tag, value: T}`.
Multi-field variants are `{tag, values: [T0, T1, ...]}`, with a TypeScript tuple
for `values`. Thus Option is `{tag: "None"} | {tag: "Some", value: T}` and
Result is `{tag: "Ok", value: T} | {tag: "Error", value: E}`. Nested Options
retain every tag, including Some(None) and Some(unit). Nominal `via` types
expose their representation type.

`entries.js` exports the Want list, launch identity and `bind(client)`, whose
result supplies `call.<name>` and `subscribe.<name>` entry stubs. Names are the
percent-decoded final component of an entry's definition identity. Duplicate
names cause `DuplicateStubName(a, b)` before any output is written.

The transport implements `call(want, argsBytes, resultDecoder)`, returning
`Promise<Outcome<R>>`, and `subscribe(want, argsBytes, onRows, onEnd?)`, returning a
handle with `unsubscribe()`. The generated stub forwards the optional `onEnd`
callback. The runtime removes the matching subscription before delivering the
decoded `Ended` reason once. An empty completed Batch delivers `[]` to `onRows`.
Reset and reconnect resubscribe without calling `onEnd`.

Compute and Granted calls return `Promise<Outcome<R>>`. Transact calls return
`Promise<Outcome<Result<R, E>>>`: the Returned payload contains the complete
Result tag and payload, including a typed Error after an aborted transaction.
Described.result is the exact Returned payload schema, including Result for
Transact. There is no separate error schema. The child fingerprints this full
result schema, so changing E changes result_fp too.

The launch identity contains digest bytes, target and `n_place`. Fingerprints
come from the described program; the generator does not implement fingerprinting.

`frame.js` exports the Frame, Outcome, Fault and Refusal codecs, the fixed Raised
and BadKind number tables, and `subprotocol = "hirsel.1"`. Frame is a protocol
type shared by every executable. The generator computes
`std.codec.schema(of[runner.Frame]())` from the compiler's bundled runner and
std sources. Describe contains only per-program data. A Frame change requires
a subprotocol change; the client checks the negotiated subprotocol.
Outcome has Returned, Failed, Refused, Abandoned and InDoubt at tags 0 through
4. Refusal appends RequestMismatch at tag 6 and HistoryUnavailable at tag 7.
Superseded is absent. The generated codecs follow the bundled declarations.

`--place-timeout` is a positive number of seconds, default 10. `--n-place` fixes
the registration allowance, default 16 MiB. Ordinary native build options select
the runtime, engine and target. A built executable can also be supplied as the
program. Describe and generation errors leave the output destination untouched.
The CLI uses the driver's shared bounded describe child. Capacity and duplicate
registration errors retain their typed payloads; an expired child reports
`PlaceTimedOut` after the launcher kills and reaps it.

## Shared client runtime

`@hirsel/client` lives in `sdk/client`. Its shared state machine consumes the
generated Frame codec and Want list. Browser storage uses strict IndexedDB
transactions and generation-scoped Web Locks; Node storage uses durable file
replacement and `flock` leases. Generation validation and each work send share
the installation lock. Missing allocated stream records fence the generation;
lost pending requests never migrate to a new principal.

Pending identity and admission uncertainty are durable before send. Returned
and Failed outcomes commit together with the next position. A definitive V1
refusal reuses the position only after a single, unambiguous attempt; otherwise
the stream retires. HistoryUnavailable retires only a matching pending request,
while RequestMismatch also fences the generation. Incompatible closes remain
terminal until an explicit reconnect or bindings reload. Reconnection waits for
all Placed frames, recovers pending calls, and restores subscriptions using new
ids. Cache replacement waits for Batch.complete.
