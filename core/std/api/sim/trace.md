# Simulation trace export (SM1)

A simulation shell can record its protocol transitions for TLC trace
validation or model-based testing with Quint and Apalache. Start one
`Tape[Event, Action, State]` per run with `start(seed)`. All three types
implement `codec`. Keep the tape in `Some` when capture is enabled and use
`None` to skip capture entirely. At each transition call
`record(mut capture, event, actions, state)` with the event, ordered action
list, and observed state after the transition. A call with `None` encodes
and allocates nothing. Only a complete encoded row enters the tape.

`ndjson(tape)` writes one JSON object per row, followed by a newline, with
`seed`, zero-based `index`, `event`, `actions`, and `state`. The integers
are exact decimal JSON tokens. Readers must parse them without converting
to binary64. A trace with no rows produces no lines.

`itf(tape, types)` writes an
[ADR-015 Informal Trace Format](https://apalache-mc.org/docs/adr/015adr-trace.html)
object. It includes the seed in trace `#meta`, the variables `event`,
`actions`, and `state` in `vars`, and the same rows in `states` with
zero-based `#meta.index` metadata. All integer expressions are
`{"#bigint":"<decimal>"}`, including small integers. Pass
`Some(Types(event, actions, state))` with Apalache's variable type strings
to include its required `#meta.varTypes`. Pass `None` to omit the type
annotations.

Both exports use the tape's codec schemas and preserve row and collection
order. Records use their declared field names. Variants and `Option` and
`Result` cases use `tag` and `value`. Zero or multiple variant fields use
a tuple payload. NDJSON writes tuples and map entries as arrays. ITF tags
tuples with `#tup` and maps with `#map`. A `via` codec uses its encoded
representation. ITF reserves record fields beginning with `#`. Both
exports return `UnsupportedFloat` for a floating-point value and `Depth`
if a value needs 128 nested schema descents. Export only reads the tape.
