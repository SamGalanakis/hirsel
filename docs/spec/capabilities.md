# Capabilities

### CAP-1: Authority is explicit

Files, directories, network, processes, clocks, randomness, standard streams, environment
variables, tasks and credentials ([`Secret`](tls.md#tls-5-secrets)) are reached only
through operations that take a capability argument.
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
by attenuation (CAP-7), or by a tape as a substitute (CAP-9), so no program can forge one.
A `Secret` can also be sealed from bytes the program already holds
([TLS-5](tls.md#tls-5-secrets)). Sealing gives the program no authority it did not have
before, because it already held those bytes. A capability that holds state is a
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
([PROG-3](programs.md#prog-3-entry)). The run's configuration grants each parameter. A
`Secret` parameter is granted from an environment variable or a file that the launcher
reads before the run starts ([TLS-5](tls.md#tls-5-secrets)); a run
whose configuration does not grant one of them, or whose provider cannot give one on this
host (a `Dir`'s minimum host, [core](core.md#contracts-the-signatures-do-not-state)), does not start: it runs no
program code and has no host effect, and reports as
[PROG-3](programs.md#prog-3-entry) says. Nothing else in a program receives authority from
the host.

### CAP-5: Provider operations

The **provider operations** are exactly the host primitives listed in [core](core.md); the
standard library's capability modules are ordinary code over them, and a program cannot
declare another. Each is carried out by a **provider**: the implementation behind the
capability, chosen by the run's configuration (the host, a recording, a replay, a
simulation) or, for a substitute, by its tape (CAP-9). Programs cannot tell providers apart
except by the values the operations return. An engine may call a provider in more than one
form, never with different behaviour: a provider that forwards every operation to another
is indistinguishable from it. A provider operation that may wait is declared `waits` and is a cancellation point
at its issue ([CONC-7](tasks.md#conc-7-cancellation-points)). Provider operations state
their allocation in their result types as every operation does
([MEM-21](budgets.md#mem-21-admission)); host memory behind them is not charged. Core task,
channel and `Arc` operations are not provider operations; their choices are events
([CONC-10](tasks.md#conc-10-event-order)).

A write that commits part of its bytes returns how many ([core](core.md)), and the rest is
sent from there. When `stdout` takes only `C3` of `C3 A9 78` and returns `written` 1, the
next call sends `A9 78`, each byte exactly once:

```hirsel
fn write_all(out: Stdout, b: Bytes) -> Result[(), WriteError] waits {
  let mut done: u64 = 0
  while done < list.len(b) {
    match stdout.write_bytes(out, list.slice(b, done, list.len(b))) {
      Ok(()) => { done = list.len(b) },
      Error(e) => { if e.written == 0 { return Error(e) }; done += e.written },
    }
  }
  Ok(())
}
```

A temporary directory's name starts with the bytes already in `name`. Here it is `t-`
followed by the suffix, and `name` ends as that whole name; a prefix holding `/` or a NUL
gives `Denied` and creates nothing:

```hirsel
fn scratch(d: Dir) -> Result[(Dir, String), IoError] waits {
  let mut name = string.new()
  match string.reserve(mut name, 34) { Ok(()) => {}, Error(_) => return Error(Other(0)) }
  match string.push(mut name, "t-") { Ok(()) => {}, Error(_) => return Error(Other(0)) }
  let dir = host.mkdtemp(d, mut name)?
  Ok((dir, take name))
}
```

A file replaces another durably when its bytes are synced before the rename and the
directory after it ([core](core.md)); a crash at any point leaves `snap` holding the old
bytes or the new, and the new once `sync_dir` has returned `Ok(())`:

```hirsel
fn replace(d: Dir, b: Bytes) -> Result[(), IoError] waits {
  let mut f = host.open(d, "snap.tmp", Write)?
  match host.write(mut f, b) { Ok(()) => {}, Error(e) => return Error(e.error) }
  host.sync(mut f)?
  host.close(take f)?
  host.rename(d, "snap.tmp", "snap")?
  host.sync_dir(d)
}
```

Random bytes land in capacity the caller already holds, so a draw admits nothing, and a
replay supplies the same bytes:

```hirsel
fn store_id(r: Random) -> Result[Bytes, Capacity] {
  let mut b: Bytes = list.with_capacity(16)?
  host.random(r, mut b)
  Ok(b)
}
```

`host.stopped(p: Process) waits` completes after the process receives its first
SIGTERM or SIGINT, including a signal received before the call. It is a
cancellation point at entry and while waiting. Its issue and delivery pass
through [CAP-8](#cap-8-the-replay-point), so replay observes the recorded
completion. Native live and recording entry install the handlers before
entering a program that receives `Process`. A second SIGTERM or SIGINT takes
its default action and ends the process immediately. Replay and launches
without `Process` leave the host's signal dispositions untouched.

### CAP-6: No ambient input

No operation without a capability argument observes anything outside its arguments: there
is no ambient clock, randomness, environment, input, thread identity or address, and no
observable hash seed. No value depends on where a block is placed in memory, and no
operation reveals the order in which `Unique` values were made
([MEM-18](memory.md#mem-18-pools-are-library-code)). The `Map` hash seed enters at the
replay point and no value depends on it ([SEM-5](semantics.md#sem-5-maps)).

### CAP-7: Attenuation

A capability can be narrowed by its module's functions: a `Dir` to a subdirectory or to
read-only access, a `Net` to a subset of its rules (including its `Tls` rules,
[TLS-3](tls.md#tls-3-authority)), a `Secret` to fewer destination names, a `Tasks` to a
child limit. A narrowed
capability never permits more than its source. No operation widens a capability or
combines two into more than either.

### CAP-8: The replay point

Every input the run did not compute (a provider's results, the scheduler's choices of
[CONC-10](tasks.md#conc-10-event-order), the entry arguments) passes one **replay point**.
A provider operation passes it twice: at issue, before any host effect, and at delivery,
before its result reaches the task. A recording provider can record each value there in
order, and a replaying provider can supply the delivery without performing the issue. A
scheduler choice is recorded; the result of an event is not, since the order and the run's
other inputs determine it, so a replay that follows the recorded order recomputes every
`receive`, `try_receive`, `latest.read` and count.
Once issued, an operation delivers exactly one result
([CONC-7](tasks.md#conc-7-cancellation-points)). Given the same values, the run behaves
identically
([SEM-8](semantics.md#sem-8-determinism)). The format of a recording is not part of the
language.

### CAP-9: Tapes

A **tape** is a core resource, `Tape`, that records or replays the provider operations made
through the capabilities it substitutes. `tape.open(d, name, mode, header)` opens the tape
file `name` in the directory `d`, to record or to replay. `tape.clock`, `tape.dir`,
`tape.stdout`, `tape.stderr` and `tape.args` each take a tape and a capability and return
a **substitute**: a capability of the same type whose every operation, and every operation
on a handle made through it, reaches the tape at its issue and at its delivery (CAP-8). A
substitute differs from its source only by the values its operations return (CAP-5), so
code that takes the capability runs unchanged on either.

- Recording, the tape performs each operation through its source's provider and records its
  issue and its delivery in the order of the run.
- Replaying, the tape performs nothing: it checks each issue against the recording before
  any effect, delivers the recorded result and never reaches the source's provider, so a
  replay has no host effect. An issue that differs from the recording, in its operation, its
  object or its argument, is a divergence: it has no effect, and the tape refuses it.
- `tape.open` refuses, before anything runs, a recording made by another build, whose build
  identity, which covers every byte of the code that runs, differs from the opener's, and
  one whose argument or limit differs.
- `tape.end(take t)` waits until every operation on the tape that is in progress has
  delivered, then seals the tape. After `end`, or once the tape is dropped, the tape refuses
  every operation on a substitute or on a handle made through it, which never reaches the
  source; cleaning such a handle closes the handle behind it, if there is one, and records
  nothing. A dropped tape keeps what it recorded but is not sealed, as if the run had ended
  there.

An operation its tape refuses fails before its issue with cause `Tape`
([FAIL-2](failure.md#fail-2-causes)), which carries no reason: the reason is in the tape's
verdict and in the printed cause. A refusal is sticky: once a tape refuses one operation it
refuses every later one, so a replay that reaches the same point fails the same way.

`tape.port(mut t)` returns a `Port` and its `PortEnd`, a way out of code that holds only
substitutes that carries bytes and nothing else: `port.call(p, request, mut reply)` is a
provider operation recorded like any other, and the holder of the `PortEnd` takes each call
with `tape.serve` and replies with `tape.answer`. The format of a tape is not part of the
language.
