# Target

[API overview](../API.md). [One meaning](../../../docs/spec/programs.md#prog-6-one-meaning).

Built except `unsupported` (macOS decision D1, 2026-11-25). Core module `target` names the target a build
is for, so a program can branch on it at compile time:

```
pub type Os: key = Linux | Macos
pub type Arch: key = X86_64 | Aarch64
pub const os: Os        // supplied by the toolchain (LANG-20)
pub const arch: Arch    // supplied by the toolchain (LANG-20)
pub fn unsupported(why: String) -> Never
```

**Values.** `target.os` and `target.arch` are constants whose values the toolchain supplies
from `--target` ([LANG-20](../../../docs/spec/language.md#lang-20-constants)). They have no
initialiser in source and are the only constants that do not. Every target triple the
module names maps to one pair, whether or not Hirsel supports that target yet:

| target | `target.os` | `target.arch` |
|---|---|---|
| `x86_64-unknown-linux-gnu` | `Linux` | `X86_64` |
| `aarch64-unknown-linux-gnu` | `Linux` | `Aarch64` |
| `aarch64-apple-darwin` | `Macos` | `Aarch64` |

`Macos` in the type is not a support claim. macOS is listed in
[reserved](../../../docs/spec/reserved.md) until its native gate is green.

**Branching.** A `match` on `target.os` or `target.arch`, or an `if` on a constant
expression built from them, keeps only its taken arm in retained code, and every arm is
still checked on every target
([PROG-6](../../../docs/spec/programs.md#prog-6-one-meaning)). Binding the value to a local
first keeps every arm. So a foreign function called only in an arm the target rules out
is not in that target's reach, and its archive need not define it
([FFI-3](../../../docs/spec/foreign.md#ffi-3-archives-are-admitted-by-their-pin-before-linking)).
A granted interface still pins its archive for the target whatever the code reaches
([FFI-4](../../../docs/spec/foreign.md)).
A constant whose initialiser uses `target.os` or `target.arch` is evaluated for every
target in the table above, and a failure on any one refuses it everywhere.

**`unsupported`.** `target.unsupported(why)` has type `Never`, so it fits any arm. `why` must
be a string literal. A build whose retained code reaches it is refused for that target,
with `why`, the target and the reach chain:
`PROG-6: main reaches target.unsupported("batched submit needs io_uring") for target aarch64-apple-darwin, through app.run → queue.submit`.
It never runs: a binary never holds a reached call to it. This is how a library refuses a
target loudly, and its caller answers by branching higher up.

**Not a host query.** `target` describes the artifact, not the machine a run is on. One
binary has one value on every run and every replay, so reading it is not an authority
([CAP-1](../../../docs/spec/capabilities.md#cap-1-authority-is-explicit)) and passes no
replay point ([CAP-8](../../../docs/spec/capabilities.md#cap-8-the-replay-point)). There
is no run-time host description (OS release, core count, memory). If one is ever added, it
is a provider operation on an existing capability that passes the replay point, never a
member of `target` (macOS decision D7).

**Forward compatibility.** `target` uses only existing forms: a sum type, a constant and a
function returning `Never`. A later general compile-time evaluation may subsume it by
supplying the same constants. It must keep both rules above: every arm checked on every
target, and retention by form.
