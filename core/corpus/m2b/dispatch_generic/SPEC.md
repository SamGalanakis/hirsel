# M2b dispatch: a handler of any type, by StaticFn

Program: `core/programs/m2b/dispatch/generic.hirsel` (package `generic`). Runner
`core/tools/m2b.py` (`//:core-m2b`): the evaluator runs `case_generic_identity`; Cranelift and
LLVM run `main generic_identity`.

The spool's intended `request` and `register_as` take `named: StaticFn[F]` for any `F`, since
a request may name a function of any type (`rpc.handlers.repeat` is `fn(Bytes, u64)`), and
take its identity themselves. Today they take the identity bytes instead (HOOK-FINDINGS S2).

## generic_identity = 1
`names[F](s: StaticFn[F], want: Bytes) -> bool` compares `code.identity(s)` with `want`;
called on `code.describe(repeat)` and the bytes of `code.identity(code.describe(repeat))` at
the call site, it is true: 1 on evaluator, Cranelift and LLVM. The generic parameter uses the
same eight-byte FunctionRef word and identity dispatch as a closed StaticFn. `request` and
`register_as` can take a `StaticFn[F]` and derive the identity themselves.
