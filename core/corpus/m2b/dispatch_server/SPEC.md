# M2b dispatch: the server role alone

Program: `core/programs/m2b/dispatch/server.hirsel` (package `server`), with library module
`m2b.spool` (`spool.hirsel`) and handlers `rpc.handlers` (`rpc.hirsel`). Runner:
`core/tools/m2b.py --host` (Buck `//:core-m2b-host`): the evaluator runs `case_<name>` with
the `Dir` and `Clock` parameters bound to handle 0 of its model world; Cranelift and LLVM run
`main <name>` built with `--dir` on an empty spool directory and `Clock` granted. Each case
publishes its own request 0, serves it with the server's registry, and reads reply 0 back, so
no second process is involved. Values are derived by hand from the frame rules in
`spool.hirsel`; none was pasted from a run. `used` is 0.

A reply is valued `Done(o)` 100 + len(o), `NoCode` 1, `Bad` 2, `Refused` 3; a failure is Io
901, Full 902, Timeout 903, Garbled 904.

## solo_exchange = 104
Request: the identity of `rpc.handlers.double` and the argument "ab". The registry holds that
identity (registered by `rpc.handlers.register`), so `double("ab")` = "abab" runs under
`bound bytes 65536` and the reply is `Done("abab")`: 100 + 4. Catches: a lookup that misses a
registered identity, a reply codec that drops or pads the result.

## solo_nocode = 1
Request: the identity of the server's own `unknown`, which nobody registers. The lookup
misses: `NoCode`. Catches: a lookup that matches on a prefix or on the name alone (the
identities share `server@1/main/` with nothing registered, and `unknown`'s signature equals
`double`'s apart from the package and name).

## solo_bad = 2
A well-formed request for identity "abc" with an empty argument, whose identity length bytes
3..10 are then rewritten to 1000 (byte 3 = 232, byte 4 = 3). The field claims 1000 bytes and
three follow, so `parse` refuses it: `Bad`, without a lookup. Catches: a length that is
trusted past the end of the frame (a read out of bounds or a slice that fails).
