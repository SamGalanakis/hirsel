# Capabilities

### CAP-1: Authority is explicit

Files, directories, network, processes, clocks, randomness, standard streams, environment
variables and tasks are reached only through operations that take a capability argument.
These capability values are the only way a program affects or observes the world outside
its values. A function holds authority only through its parameters and, for closures, its
captures (CAP-3). Top-level `const` values cannot hold capabilities.

```hirsel
fn stamp(c: Clock) -> u64 { clock.now_ms(c) }
```
```hirsel
fn stamp() -> u64 { clock.now_ms() }                     // refused: CAP-1, no Clock argument
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

### CAP-3: Callable authority

A type that holds a capability declares it with `uses`: `type Logger uses Stdout { out:
Stdout }`. The declaration is checked once against its fields' declared `uses`, which
indirections and generic arguments pass through, and is read, never re-derived, by every
other check. A callable type lists the capability types its closures may capture:
`own fn(path.Path) -> Result[Bytes, IoError] uses Dir`. A closure's `uses` must include
the `uses` of every capture's type, and a callable type without `uses` admits only
closures that capture no authority. A `uses` list is compared by inclusion, so a closure
with less authority fits a type with more. In v1 a type's or callable type's `uses` is one
declared list; there is no separate private ceiling.

```hirsel
fn reader(d: Dir) -> Result[own fn(path.Path) -> Result[Bytes, IoError] uses Dir, Capacity] {
  fn(p: path.Path) -> Result[Bytes, IoError] uses Dir { file.read(d, p) }
}
```
```hirsel
fn reader(d: Dir) -> Result[own fn(path.Path) -> Result[Bytes, IoError], Capacity] {
  fn(p: path.Path) -> Result[Bytes, IoError] { file.read(d, p) }  // refused: CAP-3, captures Dir
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
([MEM-23](budgets.md#mem-23-admission)); host memory behind them is not charged. Core task,
channel and `Arc` operations are not provider operations; their choices are events
([CONC-10](tasks.md#conc-10-event-order)).

### CAP-6: No ambient input

No operation without a capability argument observes anything outside its arguments: there
is no ambient clock, randomness, environment, input, thread identity or address, and no
observable hash seed. No value depends on where a block is placed in memory. A `Map`'s hash
may be keyed by a per-run engine seed; iteration is in insertion order and no operation
reveals a hash, so the seed is unobservable and is not an input of the run.

### CAP-7: Attenuation

A capability can be narrowed by its module's functions: a `Dir` to a subdirectory or to
read-only access, a `Net` to a subset of its rules, a `Tasks` to a child limit. A narrowed
capability never permits more than its source. No operation widens a capability or
combines two into more than either.

```hirsel
fn logs(d: Dir) -> Result[Dir, IoError] { dir.narrow(d, path.of("logs")) }
```

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
