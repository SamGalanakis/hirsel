# Capabilities

A program affects or observes the world outside its values only through **capabilities**:
values that it was given and cannot make. What a function can do is visible in its
signature, and every outside input passes one point where it can be recorded and replayed.

### CAP-1: Authority is explicit

Files, directories, network, processes, clocks, randomness, standard streams, environment
variables and tasks are reached only through operations that take a capability argument.
A function holds authority only through its parameters and, for closures, its captures
(CAP-3). Top-level `const` values cannot hold capabilities.

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

A callable type lists the capability types its closures may capture with `uses`:
`fn(Path) -> Result[Bytes, IoError] uses Dir`. A closure that captures a capability has a
type that lists it, and a callable type without `uses` admits only closures that capture
none. A `uses` list is compared by inclusion, so a closure with less authority fits a
type with more. The check reads the capture list and the type only.

```hirsel
fn reader(d: Dir) -> fn(Path) -> Result[Bytes, IoError] uses Dir {
  fn(p: Path) -> Result[Bytes, IoError] uses Dir { file.read(d, p) }
}
```
```hirsel
fn reader(d: Dir) -> fn(Path) -> Result[Bytes, IoError] {
  fn(p: Path) -> Result[Bytes, IoError] { file.read(d, p) }   // refused: CAP-3, captures Dir
}
```

### CAP-4: Entry grants

The only capabilities a run starts with are the arguments of `main`
([PROG-3](programs.md#prog-3-entry)). The run's configuration grants each parameter; a run
whose configuration does not grant one of them does not start. Nothing else in a program
receives authority from the host.

### CAP-5: Providers

Every capability operation is carried out by a **provider**: the implementation behind the
capability, chosen by the run's configuration (the host, a recording, a replay, a
simulation). Programs cannot tell providers apart except by the values the operations
return. A provider operation that may wait is declared `waits` in its signature and is a
cancellation point ([CONC-7](tasks.md#conc-7-cancellation-points)); no other operation
waits. Provider operations state their allocation in their result types as every operation
does ([MEM-23](budgets.md#mem-23-admission)).

### CAP-6: No ambient input

No operation without a capability argument observes anything outside its arguments: there
is no ambient clock, randomness, environment, input, thread identity, address or hash
seed. No value depends on where a block is placed in memory.

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
A recording provider can record each value there in order, and a replaying provider can
supply the recorded values instead; given the same values, the run behaves identically
([SEM-8](semantics.md#sem-8-determinism)). The format of a recording is not part of the
language.
