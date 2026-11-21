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
by attenuation (CAP-7), so no program can forge one.
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
fn opener(d: Dir) -> own fn(String) -> Result[HostFile, IoError] waits {
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
capability, chosen by the run's configuration: the host, or a controlled provider such as
a simulation that a test configures. Programs cannot tell providers apart except by the
values the operations return. An engine may call a provider in more than one
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

Random bytes land in capacity the caller already holds, so a draw admits nothing, and they
pass the replay point like every provider result (CAP-8):

```hirsel
fn store_id(r: Random) -> Bytes {
  let mut b: Bytes = list.with_capacity(16)
  host.random(r, mut b)
  b
}
```

`host.stopped(p: Process) waits` completes after the process receives its first
SIGTERM or SIGINT, including a signal received before the call. It is a
cancellation point at entry and while waiting. Its issue and delivery pass
the replay point ([CAP-8](#cap-8-the-replay-point)). Native entry installs the
handlers before entering a program that receives `Process`. A second SIGTERM
or SIGINT takes its default action and ends the process immediately. A launch
without `Process` leaves the host's signal dispositions untouched.

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
[TLS-3](tls.md#tls-3-authority)), a `Secret` to fewer destination names, an `Env` to fewer
variable names ([env](../../core/std/api/env.md)), a `Tasks` to a
child limit. A narrowed
capability never permits more than its source. No operation widens a capability or
combines two into more than either.

### CAP-8: The replay point

Every input the run did not compute passes one **replay point**: each provider operation's
result, each scheduler choice of [CONC-10](tasks.md#conc-10-event-order), and the entry's
arguments, which include its grants, its root limit and its launch metadata
([launch](launch.md)). A provider operation passes it twice: at issue, before any host
effect, and at delivery, before its result reaches the task. Each input enters the run once,
at its own point, and nothing outside the run reaches a value except through such a point.
A scheduler choice is an input; the result of an event is not, since the order and the
run's other inputs determine it, so the same choices give the same `receive`,
`try_receive`, `latest.read` and count. Once issued, an operation delivers exactly one
result ([CONC-7](tasks.md#conc-7-cancellation-points)). Given the same values at the
replay point in the same order, the run behaves identically
([SEM-8](semantics.md#sem-8-determinism)).

The replay point is a position in every run, not a mechanism: a build without recording
machinery has the same points and does nothing at them beyond the operation itself. They
are where a recording build observes each input once (CAP-9). Passing the replay point makes
nothing durable.

### CAP-9: Recording is outside the program

No program operation, type or cause takes, returns, names or reports a recording or a
replay. A **recording** is a trace of a run made without altering the code: the launcher or
the test runner makes it from a **recording build** of the unmodified program, and replay
is a tool run against a recording. Both are specified with the tools
([recording and replay](../recording.md)), with the recording's format, which recordings a
replay accepts, how a replay compares what the run does with what was recorded, and its
verdicts.

Recording is an optional switch, off by default, and independent of every other build
choice. A build without the switch contains no recording machinery. A recording build is
the same program, for the same target and engine at the same optimisation, with that
machinery added; it runs with recording off or on. There is no other build split: no build
mode, optimisation level or debug setting turns recording on or off, or changes what a
program means ([PROG-6](programs.md#prog-6-one-meaning)). In all three (no switch, the
switch with recording off, and with it on), a run given the same values at the replay point
(CAP-8) in the same order makes the same observations
([SEM-8](semantics.md#sem-8-determinism)): its values, output and outcome, its causes and
their sites, its charges and `Capacity` refusals, its steps and slice ends, its events, and
its provider operations and their arguments. A recording observes the run and decides
nothing in it: it charges no limit, takes no step, adds no event, poll or provider
operation, and changes no delivery. When a recording fails, the run goes on unchanged and
the tool reports it; when a replay diverges, the tool ends that replay and reports where.
Neither reaches the program.

A recording is for diagnosis and testing and makes no durability promise: nothing resumes
from one, and a crash ends it where it falls. The store is the one durability contract: a
commit to a disk store survives any crash once `store.durable` has returned for it
([store](../../core/std/api/store.md)), and nothing else promises that work survives a
crash. `host.sync` and `host.sync_dir` are the host barriers beneath it
([core](core.md#contracts-the-signatures-do-not-state)). A program whose work must survive
a crash without repeating an outside effect wraps each such effect in the store's step
library, which commits a started row before the effect and its result before the result is
used ([runner](../../core/std/api/runner.md#store-triggers)).

**Simulation.** A test that needs controlled inputs grants a simulation provider (CAP-5);
it never records from inside the program, since no program type is a tape. The simulation
provider is the **model world**. On the evaluator it is the whole host: `Args`, `Stdout`,
`Stderr`, a virtual `Clock`, `Dir` and its files, `Random` and `Net`, each capability
parameter of a law or case bound to a fresh one (`crates/hirsel-core-ir/src/eval.rs`). Its
network and clock are the deterministic model of `crates/hirsel-core-net` (`model`), which
native runs select through the runtime's model provider for `Net` and `Clock`, with every
other operation going to the host. The program-held `Tape`, removed with `CauseKind::Tape`
(whose wire number stays retired), is replaced use by use:

| use of the program-held `Tape` | replacement |
|---|---|
| a substitute `Clock`, `Args`, `Stdout`, `Stderr` or `Dir` for a test | the model world on the evaluator; natively, a granted scratch directory and the host's streams |
| controlled network input and time | the model world's network and virtual clock, on the evaluator and natively |
| recording a run, its scheduling (`tape.tasks`) included, and replaying it with no host | a recording build and replay (above) |
| a program's own record of its calls, for re-execution | ordinary code over the store, as the replay corpus (`core/corpus/m2b/replay`, `m2b.journal`) and the runner's durable replies do |
