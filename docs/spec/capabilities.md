# Capabilities

### CAP-1: Authority is explicit

Files, directories, network, processes, clocks, randomness, standard streams, environment
variables, tasks, credentials ([`Secret`](tls.md#tls-5-secrets)), credential sources
([`Cred`](tls.md#tls-7-credential-grants)), native code
([`Foreign[I]`](foreign.md#ffi-4-the-foreign-capability)) and child processes
([`Exec`](exec.md#exec-2-launch-profiles-and-narrowing)) are reached only
through operations that take a capability argument.
These capability values are the only way a program affects or observes the world outside
its values. A function holds authority only through its parameters (CAP-3). Top-level `const` values cannot hold capabilities.
These bounds bind the run's own code. A child process that an `Exec` grant starts is not
bound by them, and the grant confers what its programs can do
([EXEC-1](exec.md#exec-1-an-exec-grant-confers-what-its-programs-can-do-and-hirsel-does-not-confine-them)).

```hirsel
fn stamp(c: Clock) -> u64 { host.mono_ns(c) }
```
```hirsel
fn stamp() -> u64 { host.mono_ns() }                     // refused: CAP-1, no Clock argument
```

### CAP-2: Capabilities are opaque

A capability type is an opaque type ([PROG-2](programs.md#prog-2-visibility)) of the
standard library. Its values are made only by its own module, from entry grants (CAP-4) or
by attenuation (CAP-7), so no program can forge one. `Foreign[I]` is made only from an
entry grant ([FFI-4](foreign.md#ffi-4-the-foreign-capability)).
A `Secret` can also be sealed from bytes the program already holds
([TLS-5](tls.md#tls-5-secrets)). Sealing gives the program no authority it did not have
before, because it already held those bytes. A capability is also made by **virtual
construction**: `provide.dir`, `provide.net`, `provide.stdout`, `provide.stderr`,
`provide.clock`, `provide.random`, `provide.process` and `provide.exec`
([EXEC-8](exec.md#exec-8-exec-at-the-replay-point-and-served-exec)) ([core](core.md#provide)) each make
a fresh capability whose provider is local state of the run, with the endpoint that answers
it (a `Served` port, a `ClockHand` or a `StopTrigger`). Virtual construction grants no host
authority: what such a capability reaches is only what the program that answers it already
reaches (CAP-1), and a virtual `Net` never authorizes a granted `Secret`
([TLS-5](tls.md#tls-5-secrets)). A capability that holds state is a
resource ([OWN-3](ownership.md#own-3-kinds)); one that only designates authority is plain,
and copying it copies the designation, never the thing designated. A `Secret` is a
resource: each holder keeps its bytes and charges until it is dropped
([TLS-5](tls.md#tls-5-secrets)). Each capability type
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
reads before the run starts ([TLS-5](tls.md#tls-5-secrets)), and a `Cred` parameter from a
credential source that only the provider contacts ([TLS-7](tls.md#tls-7-credential-grants)); a run
whose configuration does not grant one of them, or whose provider cannot give one on this
host (a `Dir`'s minimum host, [core](core.md#contracts-the-signatures-do-not-state)), does not start: it runs no
program code and has no host effect, and reports as
[PROG-3](programs.md#prog-3-entry) says. Nothing else in a program receives authority from
the host. A configuration is refused if it supplies a grant with no matching entry parameter.

A `Dir` parameter is granted a path, or a directory the launcher resolves from the
invoker's XDG base directories (`xdg-config:<app>`, `xdg-cache:<app>`, `xdg-data:<app>`,
`xdg-state:<app>`; [launch](launch.md#launcher-resolved-directories)). The launcher creates
such a directory when it is missing, and only as its last step before `main`, after every
other grant above is given, so a run that does not start still has no host effect. A run
whose resolved directory has no base, or whose existing path is not a directory, does not
start.

At native Unix launch, a standard stream may be absent. `EBADF` while acquiring its
corresponding inherited descriptor (0 for `Stdin`, 1 for `Stdout`, 2 for `Stderr`)
still allows the grant, with a false terminal bit. A read that needs input reports
`StdinError.Other(EBADF)`. A nonempty write reports `WriteError(written: 0, error: Closed)`.
`EBADF` while acquiring a descriptor explicitly supplied in place of the inherited
one is a startup error. Once the provider owns a descriptor, `EBADF` from an
operation on it is an error under that operation's contract.

A write to a standard stream whose reader has gone (`EPIPE`) reports
`Closed` with the bytes written before it, as a write to a sealed virtual stream does
([core](core.md#provide)). `Closed` therefore means the other end is gone,
whether it was absent at launch or closed later; no write reports `Other(EPIPE)`. The process
ignores `SIGPIPE`, so the write returns rather than ending the run. A recording made before
this rule replays its recorded `Other(32)`. A socket's or child pipe's `EPIPE` is not an
`IoError`: those halves report `Reset` ([net](net.md), [exec](exec.md)).

### CAP-5: Provider operations

The **provider operations** are exactly the host primitives listed in [core](core.md). A
foreign call ([foreign](foreign.md)) is not one: it has no provider, no virtual form and no
replay point. The standard library's capability modules are ordinary code over them, and a program cannot
declare another. Each is carried out by a **provider**: the implementation behind the
capability, chosen by the run's configuration: the host, or a controlled provider such as
a simulation that a test configures. A program may also select a provider itself, by
calling the operations on a capability of virtual construction (CAP-2): the program's own
code answers them through `provide` ([core](core.md#provide)). This selects a provider for
the existing operations; it declares no new one. The caller pays the same charges and
admissions against a virtual provider as against the host for the same operations
([MEM-24](budgets.md#mem-24-logical-charges)). Programs cannot tell providers apart except by the
values the operations return. An engine may call a provider in more than one
form, never with different behaviour: a provider that forwards every operation to another
is indistinguishable from it. A provider operation that may wait is declared `waits` and is a cancellation point
at its issue ([CONC-7](tasks.md#conc-7-cancellation-points)). Provider operations state
their allocation in their result types as every operation does
([MEM-21](budgets.md#mem-21-admission)); host memory behind them is not charged, except a
`UdpSocket`'s receive pool and a group's queues ([MEM-19](budgets.md#mem-19-limits)), whose
refund waits for their recorded retirement. Core task,
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
  string.reserve(mut name, 34)
  string.push(mut name, "t-")
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
replay point and no value depends on it or on the placement salts and strong placement key derived from it
([SEM-5](semantics.md#sem-5-maps)).

### CAP-7: Attenuation

A capability can be narrowed by its module's functions: a `Dir` to a subdirectory or to
read-only access, a `Net` to a subset of its rules (including its `Tls` rules,
[TLS-3](tls.md#tls-3-authority)), a `Secret` to fewer destination names, an `Env` to fewer
variable names ([env](../../core/std/api/env.md)), a `Tasks` to a
child limit, an `Exec` to fewer profiles, argument forms, environment keys or cwd labels
([EXEC-2](exec.md#exec-2-launch-profiles-and-narrowing)). A narrowed
capability never permits more than its source. No operation widens a capability or
combines two into more than either.

`host.read_only(d: Dir) -> Dir` narrows a directory to read-only access without a host effect.

### CAP-8: The replay point

Every input the run did not compute passes one **replay point**: each provider operation's
result, each scheduler choice of [CONC-10](tasks.md#conc-10-event-order), and the entry's
arguments, which include its grants, its root limit and its launch metadata
([launch](launch.md)). The Map hash key is not among them: every run, a replay included,
draws its own from host entropy at entry, no recording holds it, and nothing observable
depends on it ([SEM-5](semantics.md#sem-5-maps), [PROG-11](programs.md#prog-11-map-index)). A provider operation passes the replay point twice: at issue, before any host
effect, and at delivery, before its result reaches the task. Each input enters the run once,
at its own point, and nothing outside the run reaches a value except through such a point,
or through a foreign call, which is not a replay point and which no recording build reaches
([FFI-8](foreign.md#ffi-8-a-program-that-reaches-foreign-code-is-never-recorded-replayed-or-reused)).
A scheduler choice is an input; the result of an event is not, since the order and the
run's other inputs determine it, so the same choices give the same `receive`,
`try_receive`, `latest.read` and count. Once issued, an operation delivers exactly one
result ([CONC-7](tasks.md#conc-7-cancellation-points)). Given the same values at the
replay point in the same order, the run behaves identically
([SEM-8](semantics.md#sem-8-determinism)).

A read readiness wait ([NET-1](net.md#net-1-socket-operation-declarations)) is a provider
operation of its own, on the half it names. At issue it passes the half and its deadline;
at delivery it passes only its outcome (ready, `TimedOut`, `Aborted` or an error), with no
bytes, length or capacity. The receive that follows is a separate operation with its own
issue and delivery. Served, issue is the port's `Ready(socket)` call and delivery is its
reply. The wait adds no ordering beyond what these two points already give any receive on
the same half.

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
machinery added; it runs with recording off or on. Coverage is a second such switch, with
the same guarantee ([coverage](../coverage.md)): a coverage build is the same program with
reached flags added that no program operation reads, and it makes the same observations as
the build without it. There is no other build split: no build mode, optimisation level or
debug setting turns recording or coverage on or off, or changes what a program means
([PROG-6](programs.md#prog-6-one-meaning)). For recording, in all three (no switch, the
switch with recording off, and with it on), a run given the same values at the replay point
(CAP-8) in the same order makes the same observations
([SEM-8](semantics.md#sem-8-determinism)): its values, output and outcome, its causes and
their sites, its charges and `Capacity` refusals, its steps and slice ends, its events, and
its provider operations and their arguments. A recording observes the run and decides
nothing in it: it charges no limit, takes no step, adds no event, poll or provider
operation, and changes no delivery. When a recording fails, the run goes on unchanged and
the tool reports it; when a replay diverges, the tool ends that replay and reports where.
Neither reaches the program.

A replay compares the program against the recording it is given, and does not establish
which execution produced it. A recording is a file of the recording user, so any process of
that user can rewrite it, including a child of a run granted `Exec`
([EXEC-8](exec.md#exec-8-exec-at-the-replay-point-and-served-exec)). A deployment that
needs recording integrity against such a child keeps the recording where the child cannot write
it, or does not record that run.

A recording is for diagnosis and testing and makes no durability promise: nothing resumes
from one, and a crash ends it where it falls. The store is the one durability contract: a
commit to a disk store survives any crash once `store.durable` has returned for it
([store](../../core/std/api/store.md)), and nothing else promises that work survives a
crash. `host.sync` and `host.sync_dir` are the host barriers beneath it
([core](core.md#contracts-the-signatures-do-not-state)). A program whose work must survive
a crash without repeating an outside effect wraps each such effect in the store's step
library, which commits a started row before the effect and its result before the result is
used ([runner](../../core/std/api/runner.md#store-triggers)).

**Simulation.** Grant the **model world** simulation provider (CAP-5) to a test that
needs controlled inputs. On the evaluator it is the whole host: `Args`, `Stdout`,
`Stderr`, a virtual `Clock`, `Dir` and its files, `Random` and `Net`, each capability
parameter of a law or case bound to a fresh one (`crates/hirsel-core-eval/src/lib.rs`). Its
network and clock are the deterministic model of `crates/hirsel-core-net` (`model`), which
native runs select through the runtime's model provider for `Net` and `Clock`, with every
other operation going to the host. Wire cause number 9 stays retired. Choose a testing or
recording tool by what the run needs:

| need | tool |
|---|---|
| a substitute `Clock`, `Args`, `Stdout`, `Stderr` or `Dir` for a test | the model world on the evaluator; natively, a granted scratch directory and the host's streams |
| controlled network input and time | the model world's network and virtual clock, on the evaluator and natively |
| recording a run and its scheduling, then replaying it with no host | a recording build and replay (above) |
| a program's own record of its calls, for re-execution | ordinary code over the store, as the replay corpus (`core/corpus/m2b/replay`, `m2b.journal`) and the runner's durable replies do |
