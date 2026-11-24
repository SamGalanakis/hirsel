# Generated TypeScript bindings

`hirsel sdk ts <program> --output <dir>` builds the program and runs its describe
launch mode. It emits ES modules and `.d.ts` declarations for Node and
browsers, along with the TypeScript client runtime.
The output directory has `package.json` with `"type": "module"`, so Node loads
the same `.js` files even inside a CommonJS project.

A server using `runner` declares `runner = { major = 1 }` under
`[dependencies]` in `hirsel.toml`.

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
result supplies `call`, `subscribe` and `subscribeDecoded` entry stubs, each
namespaced by module: `api.call.<module>.<name>(args, options?)`, `api.subscribe.<module>.<name>`
and `api.subscribeDecoded.<module>.<name>`. Every module is namespaced the same
way, `main` included, and there is no flat name. Calls (compute, granted and
transact entries) appear under `call`; queries appear under `subscribe` and
`subscribeDecoded`.

An entry's **stub path** comes from its definition identity `D`, which in the
identity bytes ends at the first `[` or `(` ([PROG-9](programs.md#prog-9-code-descriptors)).
The package component is dropped, and every remaining component, each module path
component and then the function's name, is percent-decoded. A nested module path
gives nested objects: an entry `fn` in module `a.b`, with `D` =
`app@1/a/b/fn`, is `api.call.a.b.fn`. Two instances of one generic function have
one stub path.

Stub paths must be distinct, and no stub path may be a proper prefix of another,
so a module `a` with an entry `b` and a module `a.b` cannot both be exported (the
object `api.call.a.b` would be both a function and a namespace). The rule covers
every entry kind together. A clash, such as two instances of one generic function
in one module, causes `DuplicateStubName(a, b)` before any output is written,
where `a` and `b` are the two entries written `module.path::name`, for example
`DuplicateStubName(main::pick, main::pick)`.

The transport implements `call(want, argsBytes, resultDecoder)`, returning
`Promise<Outcome<R>>`.

### Subscriptions

`subscribe(want, argsBytes, onChange, onEnd?)` returns a handle with `value()`,
`rows(slot)`, `map(slot)`, `get(slot, key)` and `unsubscribe()`. The four reads
return `Pending` until the current instance completes, `Ready` with the data
from the server plus pending optimistic writes, or `Ended` after an `Ended`
frame or `unsubscribe()`. `current()` reports whether the instance has a
server image. An absent key in a ready image gives `Ready` with
`undefined`.

`value()` reads the query result. `rows(slot)` reads the rows as an array.
`map(slot)` indexes the same rows by lowercase hex of the slot key.
Raw subscriptions use the full wire key. Generated decoded subscriptions
use the ordered key encoding without the store's four-byte collection id.
`get(slot, key)` reads one row by key. The array and map each keep their
identity until that slot changes. Reads and deltas share row references. The
SDK freezes decoded plain objects, arrays and tuples, and exposes
`ReadonlyMap` and readonly byte types. Mutating a shared map or
`Uint8Array`, or any data within a map leaf, is undefined behavior.

`onChange(sub, change)` runs after the client applies the whole Batch or
recomputes the image for a local change. `change.rows` contains typed `RowChange` records with the slot
and hex key. An insertion has `new`, a deletion has `old`, and an update
has both. `change.value` carries the old and new result when its encoded
bytes change. `change.replaced` marks a completing group. Its rows still
describe the difference from the current image, and a byte-identical
completion of a ready image is silent. The first completion publishes even
when its rows and value encoding are empty. An empty value in a
non-completing group leaves that result as it was.

Reset, transport loss and terminal client states clear the public image and
detach old subscription ids immediately. When an image is cleared, its
handler receives `{invalidated: true, replaced: false, rows: []}` after all
affected handles read `Pending`. The next instance's first completion
describes only the new image, including for the same Caller. The SDK ignores
frames of lost connections and groups for retired ids. Reset and reconnect
resubscribe. The SDK delivers the optional `onEnd` once after removing the
matching subscription for an `Ended` frame.

Use `api.subscribeDecoded.<module>.<name>(args, onChange, onEnd?)` for typed
values, rows and keys. A row holds its slot name, decoded key and present
value as `Some`. The generated stub uses the appropriate slot's
`Key<i>_<s>` encoder for `get`. `codec.js` exports `Value<i>`,
`Key<i>_<s>` and `RowValue<i>_<s>`
for the query at index `i` and slot `s` in that query. Use
`api.subscribe.<module>.<name>` for numeric slots and raw byte keys.

### Optimistic writes

Pass `{optimistic(draft) { … }}` as the call options to update subscription
images while a call is queued or in flight. `draft.get(sub, slot, key)`
reads the current draft. `put(sub, slot, key, row)`, `delete(sub, slot, key)`
and `value(sub, result)` edit it. The draft uses the subscription's key type
and encoder. A row passed to `put` must match its slot and key and carry a
`Some` value. Custom subscription codecs provide `encodeRow` and
`encodeValue` for the corresponding writes.

Write synchronous functions that use the draft for reads and writes. The
client runs them in queue order over the latest server images on every
commit and whenever optimistic work enters or leaves the queue. A throwing
function loses its changes and reports its error to `onDiagnostic`. The
call continues. The functions live in memory and disappear on reload.

The SDK applies a successful outcome's server rows and removes the matching
optimistic writes in the same commit. Equal encoded rows and values stay
silent. After loss or Reset, reads remain `Pending` until the fresh instance
completes. A successful outcome arriving early keeps its overlay until every
subscription it wrote has completed or ended. Failures remove the overlay
immediately. If an instance is current and the server delivers a successful
outcome before its rows in a later Batch, the visible row can revert briefly.

Server rows and Draft writes use the same encoded key in decoded slots,
so confirmation can keep an optimistically inserted row even when the slot
was empty. The collection id belongs to the live store and can differ from
ids in Describe's empty-store declaration run.

For a query, `Described.result` is R's value schema followed by the slot list,
each slot's name, ordered key schema and value schema, each schema encoded as
`std.codec` Bytes. `result_fp` fingerprints the whole, so changing R, a slot's
name, its table's schemas, or the slot order changes the Want. Describe also
carries the explicit `value` and `slots` schemas for each query. Its
`tables` diagnostics list declarations from a Migrate run over an empty
memory store. The generator exports those diagnostics without using them as
part of the release identity.

Compute and Granted calls return `Promise<Outcome<R>>`. Transact calls return
`Promise<Outcome<Result<R, E>>>`: the Returned payload contains the complete
Result tag and payload, including a typed Error after an aborted transaction.
For calls, Described.result is the exact Returned payload schema. The child
fingerprints the full Result for Transact, so changing E changes result_fp too.

The launch identity contains digest bytes, target and `n_place`. Fingerprints
come from the described program; the generator does not implement fingerprinting.

`frame.js` exports the Frame, Outcome, Fault and Refusal codecs, the fixed Raised
and BadKind number tables, and `subprotocol = "hirsel.3"`. Frame is a protocol
type shared by every executable. The schema `std.codec.schema(of[runner.Frame]())`
is generated from the Runner and std sources by `core/tools/frame_schema.py`
and checked in as `core/programs/runner/frame-schema.bin`. The
`core-runner-frame-schema` CI target regenerates it and compares the bytes. The generator reads that file, and
Describe contains only per-program data. A Frame change requires a subprotocol
change; the client checks the negotiated subprotocol. To a client that offers
only retired tokens (`hirsel.1`, `hirsel.2`) the server answers 101 selecting the
first token offered, so the browser completes the handshake, and then Close 1009
with reason `hirsel.3` before any Welcome; the client reports that the page or client must be
reloaded, and never retries. A 1009 after Welcome is the stored-reply close and
is reported as a reply error.
Outcome has Returned, Failed, Refused, Abandoned and InDoubt at tags 0 through
4. Fault appends Conflict at tag 4 (RUNNER-SPEC v2.45). Refusal appends RequestMismatch at tag 6 and HistoryUnavailable at tag 7.
Superseded is absent. The generated codecs follow the bundled declarations.

`--place-timeout` is a positive number of seconds, default 10. `--n-place` fixes
the registration allowance, default 16 MiB. Ordinary native build options select
the runtime, engine and target. A built executable can also be supplied as the
program. Describe and generation errors leave the output destination untouched.
The CLI uses the driver's shared bounded describe child. Capacity and duplicate
registration errors retain their typed payloads; an expired child reports
`PlaceTimedOut` after the launcher kills and reaps it.

## Shared client runtime

The generated directory includes the shared client runtime in `client.ts`,
`persistence.ts`, `browser.ts` and `node.ts`. Compile these sources with your
TypeScript app or bundle them with your browser app. Include the generated
`codec.js`, `entries.js` and `frame.js` beside the compiled runtime. The SDK
needs no runtime npm dependencies.

```text
import {Client} from './generated/client.js';
import {BrowserStorage} from './generated/browser.js';
import {Frame} from './generated/frame.js';
import {bind, identity, want} from './generated/entries.js';

const client = await Client.open({
  url: 'wss://example.test/hirsel',
  frame: Frame,
  wants: want,
  identity,
  storage: new BrowserStorage(),
});
await client.whenReady();
const api = bind(client);
```

For Node 22 or later, use `NodeStorage` from `./generated/node.js` and pass
a private state directory to its constructor. The Node adapter uses `flock`
and needs Node type declarations when you type-check it.

`Client.open` requires the ingress WebSocket `url`, the generated `frame`
codec and `wants`, and a `storage` adapter. Pass the generated `identity` to
check the launch digest, target and `n_place`. Optional `credential` bytes go
in Hello. `deadlineMs` sets the server's call deadline and defaults to 30,000.
The optional `socket` factory and `clock` override the platform WebSocket and
time source. `onState`, `onDiagnostic`, `onRecovered` and `onBatch` observe
state changes, diagnostics, recovered call outcomes and applied Batch versions.

Browser storage uses strict IndexedDB transactions and generation-scoped Web
Locks. Node storage uses durable file
replacement and `flock` leases. Generation validation and each work send share
the installation lock. Missing allocated stream records fence the generation;
pending requests are tied to the Caller saved for that stream.

The client keeps one call in flight per stream. Calls submitted while ready
wait in memory in submission order, including behind a recovered call.
Pending identity and admission uncertainty are durable before send. Returned
and Failed outcomes commit together with the next position. A definitive V1
refusal reuses the position only after a single, unambiguous attempt; otherwise
the stream retires. HistoryUnavailable retires only a matching pending request,
while RequestMismatch also fences the generation. Incompatible closes remain
terminal until an explicit reconnect or bindings reload. Reconnection waits for
all Placed frames, recovers pending calls, and restores subscriptions using new
ids. The old images are invalidated at the disconnect, and each subscription is
pending until its new `complete` (above).

The application may supply credential bytes in Client options. The client
sends them in Hello and stores the returned Caller with the stream. It replays
pending work only when Welcome returns the same Caller. If an anonymous
connection receives a new anonymous Caller, the client reports any pending
outcome as LocallyUnknown and starts at position zero for that Caller.

## Calling an entry from the terminal

`hirsel call SERVER --url ws://HOST:PORT/PATH [-- ENTRY [--FLAG VALUE...]]`
lists a running place's entries, or calls one of them, for debugging. SERVER is
the built executable the place runs. Without ENTRY, or with `-- --help`, the
command prints every entry with its kind, flags and result type. `-- ENTRY
--help` also prints the entry's named types.

ENTRY is `MODULE/NAME`: the entry's module path, its components joined by `.`,
then `/` and the function's name, the stub path of the TypeScript bindings
(section 4) written for the terminal. So `fn` in module `main` is `main/fn`, and
`fn` in module `a.b` is `a.b/fn`. There is no bare-name form, for `main` or any
other module. The listing prints each entry in this form, and the stub path
rules, including `DuplicateStubName`, apply as they do in `hirsel sdk ts`.

**Transport.** The call is an ordinary client session on the server's ingress,
as the TypeScript client's is. It sends Hello without an application credential.
It sends one Call on stream 0 at position 0, then reads Batch and Reset frames until
the Done for that position arrives. There is no debug entry point or second call
path: a place applies its credential binding to the CLI's empty credential as it
would to any other client. The upgrade offers `hirsel.3` and checks
`Sec-WebSocket-Accept` and the selected subprotocol. `--timeout` bounds the
connection and is the Call's `deadline_ms`; default 10 seconds. A session that
ends after the Call was sent reports that the outcome is unknown, and the call
is never resent. `wss://` is refused as unsupported.

**Schemas.** Entry identities, fingerprints and argument and result schemas come
from SERVER's describe launch mode, as in `hirsel sdk ts`. `--n-place` and
`--place-timeout` mean what they mean there. They must match the running place,
so the types match what is running. The Frame codec is the same one the SDK
uses, from the checked-in Runner frame schema. Before sending a Call, the CLI
checks two things:

- If Welcome names a launch digest, the digest, target and `n_place` must equal
  the Describe's. Otherwise the CLI refuses and sends nothing more.
- The Hello want carries the Describe's identity and fingerprints, and the
  server answers Placed per want. Gone means the server does not serve that
  identity with those fingerprints, and the CLI refuses without sending the
  Call. So does Resolve, which the CLI does not support.

Query entries are listed, but calling one is refused as unsupported, because a
query streams rows rather than returning one outcome.

**Arguments.** Every flag is typed against the argument schema before the Frame
probe or any connection runs:

- If the argument type is a record, pass `-- ENTRY --NAME VALUE ...`, with
  one flag for each non-unit field.
- Otherwise pass the whole argument as `-- ENTRY --arg VALUE`.
- A unit argument takes no flags.

Unknown, repeated and missing flags are refused, and so is a value that does not
fit its type. A refusal names the flag and the expected type.

Primitive values are plain text:

- bool is `true` or `false`.
- Integers are decimal, with `-` only for signed types, and must fit their width
  exactly.
- f32 and f64 are a finite decimal, `NaN`, `inf` or `-inf`.
- char is one Unicode scalar.
- String is the raw text.
- Bytes is hex with an even number of digits.

A `via` type takes its representation's form. Every other value is JSON in the
one mapping below. The same mapping prints outcomes.

**The JSON mapping.** Each `std.codec` value type has exactly one JSON form:

- unit is `null`.
- bool is a JSON boolean.
- Every integer type is an exact JSON integer, including u64 and i64 values
  beyond 2^53. A fraction, an exponent or an out-of-range value is refused.
- f32 and f64 are JSON numbers, or the strings `"NaN"`, `"inf"` and `"-inf"`.
- char and String are JSON strings.
- Bytes (List[u8]) is a hex string.
- Other lists are arrays.
- Map[K, V] is an array of `[key, value]` pairs; duplicate keys are refused.
- Tuples are arrays of their exact arity.
- Records are objects with exactly their live field names. Key order carries no
  meaning, and output keys are sorted.
- Variants, Option and Result use the tagged form of section 4: `{"tag": N}`,
  `{"tag": N, "value": V}` or `{"tag": N, "values": [V0, V1, ...]}`.

**Output.** The CLI prints the Outcome as one JSON line on stdout, in the same
mapping. For Returned, the payload is decoded with the entry's result schema,
for example `{"tag":"Returned","value":{"tag":"Ok","value":4}}`. Other outcomes
print as the Frame codec decodes them, for example
`{"tag":"Failed","value":{"tag":"Raised","value":1}}`. The exit status is 0 for
Returned and 1 for any other outcome. Refusals before sending, and transport
errors, exit 2 with a message on stderr.

Design notes:

- Integers must be exact JSON integers, not strings or a second syntax, so there
  is one form for every type and nothing silently rounds through f64.
- Primitive flags take plain text because it is unambiguous for those types.
- Composite values are JSON rather than Hirsel literal syntax: the driver
  already parses JSON, and section 4 already defines the tagged shapes.
- The command is `call`, beside `sdk`, because both read a server's Describe.
