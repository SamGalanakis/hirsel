# Capabilities

### CAP-1: Authority is explicit

Files, directories, network, processes, clocks, randomness, standard streams, environment
variables and tasks are reached only through operations that take a capability argument.
These capability values are the only way a program affects or observes the world outside
its values. A function holds authority only through its parameters (CAP-3). Top-level `const` values cannot hold capabilities.

```hirsel
fn stamp(c: Clock) -> u64 { host.mono_ns(c) }
```
```hirsel
fn stamp() -> u64 { host.mono_ns() }                     // refused: CAP-1, no Clock argument
```

### CAP-2: Capabilities are opaque

A capability type is an opaque type ([PROG-2](programs.md#prog-2-visibility)) of the
standard library. Its values are made only by its own module, from entry grants (CAP-4) or
by attenuation (CAP-7), so no program can forge one. A capability that holds state is a
resource ([OWN-3](ownership.md#own-3-kinds)); one that only designates authority is plain,
and copying it copies the designation, never the thing designated. Each capability type
declares whether it is `send` ([CONC-2](tasks.md#conc-2-the-send-bound)).

```hirsel
fn forge() -> Dir { Dir(fd: 3) }                         // refused: PROG-2, Dir is opaque
```

### CAP-3: Authority is received

A function's authority is exactly the capability values it receives as arguments, directly
or inside argument values (fields, collections, counted payloads). There are no authority
summaries in v1: no declaration lists the capabilities a type or a function may hold, and a
generic `T` may carry any. An owning closure captures no capability
([LANG-14](language.md#lang-14-captures)), so a callable value holds no authority of its own
and reaches the world only through the arguments of each call; a scoped callback uses the
receiving call's capabilities for that call. A later authority facade is reserved
([reserved](reserved.md), R19).

```hirsel
fn load(d: Dir, p: String, f: fn(Dir, String) -> Result[u64, IoError] waits) -> Result[u64, IoError] waits {
  f(d, p)
}
```
```hirsel
fn opener(d: Dir) -> Result[own fn(String) -> Result[HostFile, IoError] waits, (Capacity, Dir)] {
  fn(p: String) -> Result[HostFile, IoError] waits { host.open(d, p, Read) }   // refused: LANG-14, captures Dir
}
```

### CAP-4: Entry grants

The only capabilities a run starts with are the arguments of `main`
([PROG-3](programs.md#prog-3-entry)). The run's configuration grants each parameter; a run
whose configuration does not grant one of them does not start. Nothing else in a program
receives authority from the host.

### CAP-5: Provider operations

The **provider operations** are exactly the host primitives listed in [core](core.md); the
standard library's capability modules are ordinary code over them, and a program cannot
declare another. Each is carried out by a **provider**: the implementation behind the
capability, chosen by the run's configuration (the host, a recording, a replay, a
simulation). Programs cannot tell providers apart except by the values the operations
return. A provider operation that may wait is declared `waits` and is a cancellation point
at its issue ([CONC-7](tasks.md#conc-7-cancellation-points)). Provider operations state
their allocation in their result types as every operation does
([MEM-21](budgets.md#mem-21-admission)); host memory behind them is not charged. Core task,
channel and `Arc` operations are not provider operations; their choices are events
([CONC-10](tasks.md#conc-10-event-order)).

### CAP-6: No ambient input

No operation without a capability argument observes anything outside its arguments: there
is no ambient clock, randomness, environment, input, thread identity or address, and no
observable hash seed. No value depends on where a block is placed in memory, and no
operation reveals the order in which `Unique` values were made
([MEM-18](memory.md#mem-18-pools-are-library-code)). The `Map` hash seed enters at the
replay point and no value depends on it ([SEM-5](semantics.md#sem-5-maps)).

### CAP-7: Attenuation

A capability can be narrowed by its module's functions: a `Dir` to a subdirectory or to
read-only access, a `Net` to a subset of its rules, a `Tasks` to a child limit. A narrowed
capability never permits more than its source. No operation widens a capability or
combines two into more than either.

### CAP-8: The replay point

Every input the run did not compute (a provider's results, the scheduler's choices of
[CONC-10](tasks.md#conc-10-event-order), the entry arguments) passes one **replay point**.
A provider operation passes it twice: at issue, before any host effect, and at delivery,
before its result reaches the task. A recording provider can record each value there in
order, and a replaying provider can supply the delivery without performing the issue.
Once issued, an operation completes and delivers. Given the same values, the run behaves
identically
([SEM-8](semantics.md#sem-8-determinism)). The format of a recording is not part of the
language.
