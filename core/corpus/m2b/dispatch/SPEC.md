# M2b dispatch: two cooperating processes

Program: `core/programs/m2b/dispatch/` (`server.hirsel`, `client.hirsel`, library
`m2b.spool`, handlers `rpc.hirsel` and `rpc_changed.hirsel`). Runner: `core/tools/m2b.py
--host`, case kind `pair`: for Cranelift and LLVM it builds the case's `server` and `client`
workloads against one empty spool directory, starts the server with `serve` (it answers
request 0 and prints 1), runs the client with the case's arguments (default: the case name),
and passes when the client prints `value`, the server prints `server_value` and neither
writes to stderr. The evaluator has no second process; each role runs alone there
(`../dispatch_server`, `../dispatch_client`). Values are derived by hand from the frame rules
in `spool.hirsel` and the handlers' source; none was pasted from a run.

Reply values: `Done(o)` 100 + len(o), `NoCode` 1, `Bad` 2, `Refused` 3; failures Io 901,
Full 902, Timeout 903, Garbled 904. Every server prints 1: it answered one request, whatever
the reply was.

## exchange = 104
`rpc.handlers.double("ab")` by its identity: `Done("abab")`, 100 + 4.

## typed = 106
`rpc.handlers.repeat` takes `(Bytes, u64)`, not the registry's type, so the server registers
a byte adapter under `repeat`'s identity. The argument is the count 3 as 8 little-endian
bytes, then "ab": `repeat("ab", 3)` = "ababab", 100 + 6. Catches: an adapter that decodes the
count or the bytes wrongly (a count read as 0 gives 100, the count bytes left in give 120).

## nocode = 1
The client's own `triple`, which the server never registered: `NoCode`.

## bad = 2
The frame "HQ" with version 2: `Bad`, without a lookup.

## changed = 1
The client sends `typed` (the identity of `repeat(view Bytes, view u64)`) to the server built
against `rpc_changed.hirsel`, where `repeat`'s count is `u32`. The changed server registers
the identity of `repeat(view Bytes, view u32)`; the old identity is not in its registry, so
the old request is refused as `NoCode` instead of being decoded with the new signature.
Catches: an identity that leaves out the parameter types, which would make the server run the
new `repeat` on an argument laid out for the old one.
