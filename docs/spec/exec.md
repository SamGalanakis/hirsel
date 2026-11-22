# Process creation

A program starts a native process only through an `Exec` capability. An `Exec` grant names
**launch profiles** that the operator approved when starting the run. Each profile pins an
executable image by its digest and lists the argument forms, environment and working
directories a child may receive. The program supplies literal argument bytes and owns the
child through a `Proc` handle, with optional pipes on the child's standard streams.

The rules are EXEC-1 to EXEC-8. These rules are unbuilt design. Every fence on this page is
marked `hirsel unbuilt`, and the laws of each rule state what the implementation must make
true.

## Trust

### EXEC-1: An Exec grant confers what its programs can do, and Hirsel does not confine them

A child process is not contained. An `Exec` grant gives the run whatever each named program
can do with the argument forms, environment and working directories its profile allows.
For a general program this is the whole authority of the OS user the run runs as. Hirsel
promises exactly these things about a child, and nothing else:

1. **Image.** The bytes that run are the bytes whose SHA-256 the profile pins (EXEC-4).
2. **Inputs.** The child receives exactly the argv, environment and working directory that
   EXEC-3 derives from the profile and the call, and the descriptors 0, 1 and 2 that EXEC-3
   names, and no other descriptor of the run.
3. **Run integrity.** The child cannot read or write the run's memory or use the run's
   descriptors (EXEC-7). What a child does reaches the run only through the operations of
   this chapter and through host state that later provider operations read. So every
   effect of a child on the run passes the replay point
   ([CAP-8](capabilities.md#cap-8-the-replay-point)).
4. **Process tree.** No process the child starts outlives the run, except one that an
   outside service starts on the child's request (EXEC-6).

A child's effects on the host are bounded only by the OS. The capability bounds of the run
bind the run's own code and not its children:

- a child may read and write files outside every granted `Dir`;
- it may reach the network outside every `Net` rule;
- it may read the source of a granted `Secret` ([TLS-5](tls.md#tls-5-secrets));
- it may signal other processes of the same user, including the run, which then sees
  SIGTERM or SIGINT through `host.stopped` and SIGKILL as environmental termination
  ([FAIL-15](failure.md#fail-15-run-stops)).

The program can direct a child through its arguments and its stdin. So a run granted `Exec`
for a general program has, through that program, the authority of its OS user. Argument forms,
environment rules and cwd labels bound what reaches the image. They do not bound what the
image does with it.

**The operator.** An `Exec` grant is explicit at launch. It is given on the command that
starts the run: `--grant 'Exec=FILE NAME[,NAME]'` for `hirsel run`, `hirsel test` and
`hirsel deploy`. It is never given only by a package's configuration or a recipe. These follow
[FFI-4](foreign.md#ffi-4-the-foreign-capability)'s rules for `Foreign=D` with `Exec=NAME` in
its place:

- `hirsel test` grants a test's configured `"Exec=FILE NAME"` only when its own command line
  also holds the grant;
- `hirsel deploy` refuses a recipe with an `Exec` binding unless the request holds it.

Before any process runs with the grant, the launcher writes one line per profile to stderr
(for `hirsel deploy`, to the deploy's output):
`exec: NAME: image PATH, sha256 E, interpreter I`. `I` is the image's `PT_INTERP` path, or
`none` for a static image. The operator who passes the grant has seen what it trusts.

A contained child, confined to its profile's files, network and descendants by the host, is a
separate later design ([reserved](reserved.md)). No Linux mechanism that unprivileged
processes have expresses [NET-6](net.md#net-6-network-rules)'s address rules for another
process. A profile that claimed such a bound on a host that cannot enforce it would be
unsound, so no profile claims one.

## Profiles and authority

### EXEC-2: Launch profiles and narrowing

`Exec` is a core capability. It is plain, `shareable` and `send`, and 8 bytes, and it
designates a set of launch profiles. A profile has these parts:

- a **name**;
- an image path and its pinned SHA-256;
- `argv0`;
- a list of **argument forms**;
- environment rules;
- a set of **cwd labels**, each mapping to a host directory.

A profile file is UTF-8 text in TOML, one table per profile:

```text
[git]
image = "/usr/bin/git"
sha256 = "<64 lowercase hex digits>"
argv0 = "git"
args = [["status", "--porcelain"], ["log", "-n", "{any}", "{rest}"]]
env = { LANG = "C.UTF-8", GIT_AUTHOR_NAME = "{any}" }
dirs = { work = "/srv/repo" }
```

An argument form is a list of tokens. A literal string token matches exactly those bytes,
`{any}` matches any one argument, and `{rest}`, allowed only last, matches zero or more
further arguments. A literal token cannot be spelled `{any}` or `{rest}`. Environment rules
come in two kinds. A fixed value (`LANG`) is always passed and cannot be overridden. A key
marked `{any}` takes the caller's value, or is absent when the caller gives none. Keys are
non-empty and hold no `=` or NUL. Every profile has at least one cwd label.

The launcher reads the profile file before the run starts. A missing file, a malformed
profile, an unknown name, an image whose bytes do not match the pin, or an image that starts
with `#!` makes the run not start ([CAP-4](capabilities.md#cap-4-entry-grants)), and its one
line names the profile and the cause. A script runs through its interpreter's profile, with
the script path as a literal argument, and that path is outside the pin.

```hirsel unbuilt
pub type ArgToken = Lit(Bytes) | Any | Rest
pub type ExecRule = Programs(names: String) | Form(program: String, form: List[ArgToken]) | EnvKeys(program: String, keys: String) | Dirs(program: String, labels: String)

pub fn narrow(e: Exec, r: ExecRule) -> Result[Exec, RuleError]
```

`exec.narrow` intersects each profile with `r` (comma-separated names, keys and labels):

- `Programs` keeps the listed profiles.
- `Form` replaces the named profile's forms with their intersections with `form`, and drops
  the empty ones. Two forms intersect position by position:
  - equal literals give that literal, and unequal ones give nothing;
  - a literal and `Any` give the literal;
  - `Rest` against the other form's remaining tokens gives those tokens, and `Rest` against
    `Rest` gives `Rest`;
  - forms of different lengths without a `Rest` give nothing.
- `EnvKeys` keeps the named profile's listed keys. A dropped key is then never passed, fixed
  or not.
- `Dirs` keeps the named profile's listed labels.

A profile left with no form or no label is dropped. A `Rest` anywhere but last, or an empty
or malformed list, gives `Invalid` and changes nothing. Rule storage is admitted as
`net.narrow` admits it ([NET-6](net.md#net-6-network-rules)).

Law: for every profile `P`, rule `R` and launch `L`, the narrowed profile admits `L` exactly
when `P` admits `L` and `R` allows it. `narrow` never admits a launch its source refuses
([CAP-7](capabilities.md#cap-7-attenuation)).

## Spawning

### EXEC-3: Spawn

```hirsel unbuilt
pub opaque resource type Proc
pub type Stdio = Discard | Piped
pub type Launch {
    program: String,
    args: List[Bytes],
    env: List[(String, Bytes)],
    dir: String,
    path: String,
    stdin: Stdio,
    stdout: Stdio,
    stderr: Stdio
}
pub type Spawned = Spawned(Proc, Option[WriteHalf], Option[ReadHalf], Option[ReadHalf])
pub type ExecError = Denied | TimedOut | Aborted | Other(u32)

pub fn spawn(e: Exec, l: Launch) -> Result[Spawned, ExecError] waits
```

`exec.spawn` checks the launch against `e` before issue. Each check that fails gives `Denied`
with no host effect. The launch must satisfy all of these:

- `program` names a profile of `e`;
- `args` matches one of its forms, and no argument holds a NUL;
- every `env` key is a key the profile marks `{any}`, and no key appears twice;
- no value holds a NUL;
- `dir` is one of its labels;
- `path` is empty or a valid path ([core](core.md#contracts-the-signatures-do-not-state)).

The child receives exactly:

- **argv** `argv0` followed by `args`.
- **envp** the profile's fixed values and the caller's `{any}` values. Nothing is inherited
  from the run's environment, and the order is bytewise by key.
- **cwd** the label's directory, or the directory `path` names beneath it. That directory is
  resolved as a `Dir` path is, with no symbolic link and no escape. A path that does not
  resolve to a directory gives `Denied`.
- **descriptors** on 0, 1 and 2, `/dev/null` for `Discard`, or a new pipe for `Piped`.
  Nothing else is inherited.
- **signals** default dispositions and an empty signal mask.
- **search** no `PATH` search and no shell. There is no privilege change.

For each `Piped` stream the result holds the run's end, a `WriteHalf` for stdin and a
`ReadHalf` for stdout and stderr, and `None` for each `Discard`. These are the halves of
[NET-7](net.md#net-7-owned-connections): `host.send_pair` and `host.recv_bounded` move their
bytes with the same partial commits, deadlines and cancellation, and dropping a `WriteHalf`
closes the pipe, so the child reads end of input. A write to a pipe whose reader has closed
gives `Reset`. `net.join` refuses a pipe's halves (NET-7 check 1), so no pipe reaches TLS.
`Proc` is a mechanical resource of logical size 8, `send`, move-only and never `shareable`. A
spawn admits its handles as `host.connect` admits a socket
([MEM-24](budgets.md#mem-24-logical-charges)).

`spawn` is a cancellation point at its issue only, and the host provider completes it at its
issue ([CONC-7](tasks.md#conc-7-cancellation-points)). A child that the host cannot start,
including an exec failure reported after fork, gives `Other(errno)` and leaves no process.
`TimedOut` and `Aborted` come only from a served Exec (EXEC-8).

```hirsel unbuilt
fn status(e: Exec, out: Stdout, until: Until) -> Result[u64, ExecError] waits {
  let l = Launch(program: "git", args: list.new(), env: list.new(), dir: "work", path: "", stdin: Discard, stdout: Piped, stderr: Discard)
  let Spawned(p, _, o, _) = exec.spawn(e, l)?
  let mut buf: Bytes = list.with_capacity(4096)
  let mut read = option.unwrap(take o)
  let mut total: u64 = 0
  loop {
    match host.recv_bounded(mut read, mut buf, 4096, until) {
      Ok(0) => break,
      Ok(n) => { total += n; list.clear(mut buf) },
      Error(_) => return Error(Other(0)),
    }
  }
  let mut proc = take p
  match exec.wait(mut proc, until)? {
    Exited(0) => Ok(total),
    _ => Error(Other(1)),
  }
}
```

### EXEC-4: The image is the pinned bytes

At launch, for each granted profile, the launcher:

1. opens the image path;
2. copies its bytes into a new sealable anonymous file;
3. checks the copy's SHA-256 against the pin;
4. seals the copy against every write, shrink and grow.

Every spawn of that profile executes that sealed copy by descriptor (`execveat` with
`AT_EMPTY_PATH`). Renaming, replacing or rewriting the image file after launch, by any
process, changes nothing that runs. The pin covers the main image's bytes. A dynamically
linked image's interpreter and shared libraries are host files resolved at each exec and are
not pinned. Their path appears on the approval line (EXEC-1). The sealed copy is host memory
outside every limit and lives for the run.

## Owning a child

### EXEC-5: Wait and signal

```hirsel unbuilt
pub type Termination = Exited(u8) | Signalled(u8)
pub type Signal = SigInt | SigTerm | SigKill

pub fn wait(mut p: Proc, until: Until) -> Result[Termination, ExecError] waits
pub fn signal(mut p: Proc, s: Signal) -> Result[(), ExecError] waits
```

`exec.wait` waits until the child has ended and has been reaped. It returns `Exited(code)`
for an exit and `Signalled(number)` for termination by a signal. A nonzero code is a
successful wait. Once a child has ended, every later `wait` returns the same value at once.
`wait` waits after issue: it is a cancellation point at issue and while it waits. Cancellation
there gives `Aborted`, and a passed `until` gives `TimedOut`. Either way the child is
unchanged and `p` still owns it.

`exec.signal` sends SIGINT, SIGTERM or SIGKILL to the child through a stable process handle
(a pidfd), never by process ID, so it never reaches another process. `Ok(())` says the request
was made. After the child has ended it sends nothing and returns `Ok(())`. `signal` completes
at its issue. There is no operation on arbitrary process IDs, process groups or a detached
child. A lifetime limit is library code: signal `SigTerm`, wait with a deadline, then signal
`SigKill` and wait.

### EXEC-6: Dropping a Proc ends its process tree

Dropping a `Proc` whose child has not been reaped sends that child SIGKILL and hands it to the
run's supervisor, which reaps it. The finalizer never waits. The record the supervisor needs is
reserved at spawn, so a drop cannot fail.

A run granted `Exec` is a child subreaper. Each process that a child starts and orphans
becomes a child of the run, so every descendant stays in the run's process tree until it
dies. The supervisor kills and reaps each child of the run that no live `Proc` owns. It looks
for them whenever a `Proc`'s child ends, is reaped or is dropped. A descendant orphaned deeper
in the tree, whose parent died while that parent's own parent lives, may run until the next
such look. When the run ends, the supervisor kills every remaining child, then repeats until
the run has no child process. Processes that an outside service starts on a child's request
(a user service manager, a scheduler, a remote shell) are not descendants and are not
covered. The runtime's own helper processes are not `Exec` children and the supervisor leaves
them alone.

### EXEC-7: A run granted Exec is out of its children's reach

Before `main`, a run granted `Exec` makes itself non-dumpable. Processes of the same user,
its children included, can then neither trace it nor open its memory, environment or
descriptors through `/proc` without the host's tracing privilege, whatever the host's tracing
policy. With EXEC-3's descriptor rule, this keeps `Secret` bytes held in run memory, the
sealed images and every run descriptor out of the children's reach. A run granted `Exec`
writes no core dump. A run without an `Exec` grant does not change these settings.

On Linux the provider needs, in addition to `Dir`'s floor:

- `pidfd_open` (Linux 5.3);
- `close_range` (Linux 5.9);
- sealable anonymous files.

A run granted `Exec` probes them once before `main`. Where one is unavailable the run does not
start, and its one line names the cause, `Exec requires pidfd_open and close_range (Linux 5.9
or later)`.

## Replay and simulation

### EXEC-8: Exec at the replay point and served Exec

**Recording.** A program that reaches `exec.spawn` may be recorded and replayed. A child
reaches the run only through recorded deliveries (EXEC-1, EXEC-7), unlike foreign code
([FFI-8](foreign.md#ffi-8-a-program-that-reaches-foreign-code-is-never-recorded-replayed-or-reused)).

- *The grant snapshot* holds each profile's name, its image digest, `argv0`, its forms, its
  environment keys each marked fixed or `{any}`, and its cwd labels. It never holds host
  paths, fixed environment values, process IDs or descriptors.
- *A spawn's issue* holds the `Exec` ordinal and the canonical launch:
  - `program` and `dir`;
  - `path`;
  - each argument with its length;
  - the environment sorted by key, each key and value with its length;
  - the three stdio modes.
- *Its delivery* holds the new `Proc` and pipe ordinals, or the error.
- *A wait* holds the `Proc` ordinal and `until` at issue, and the termination or the error at
  delivery.
- *A signal* holds the ordinal and the signal at issue, and the outcome at delivery.
- *Pipe I/O* uses the half rows ([recording](../recording.md#what-a-recording-holds)).

A replay starts no process, creates local handles, and on a drop changes local ownership only.

**Served Exec.** `provide.exec(bytes n)` makes an `Exec` of virtual construction with its
`Served[ExecOp]` port ([core](core.md#provide)). It permits every program name, form, key and
label, and `exec.narrow` narrows it.

```hirsel unbuilt
pub type ExecOp = Spawn(program: u64, args: u64, env: u64, dir: u64, path: u64, stdin: Stdio, stdout: Stdio, stderr: Stdio) | Await(proc: u64) | Signal(proc: u64, signal: Signal) | Recv(pipe: u64, room: u64, max: u64) | Send(pipe: u64, prefix: u64, tail: u64) | Released(id: u64, part: Part)
pub type ExecReply = Done | Failed(ExecError) | IoFailed(NetError) | Count(u64) | Started(proc: u64, stdin: u64, stdout: u64, stderr: u64) | Ended(Termination)

pub fn exec(bytes n: u64) -> (Exec, Served[ExecOp])
pub fn exec_op(c: Call[ExecOp]) -> ExecOp
pub fn exec_reply(r: ExecReply) -> Reply[ExecOp]
```

The served rules are Provide's: a port room, payload fields by declaration order, `fill`,
reply as the commit point, withdrawal, sealing to `Reset` (pipe I/O) or `Other(0)` (other
operations), fresh kind-checked ids issued only by a reply, and cross-provider handles
`Denied`.

- **Spawn's payloads.** `args` is each argument as a `u32` little-endian length followed by
  its bytes. `env` is each key and then its value in the same form, sorted by key.
- **Started's ids.** A `Started` reply issues the `Proc` id and exactly one pipe id for each
  `Piped` stream, and 0 for each `Discard`. Any other combination fails the server with `Fail`.
- **Await.** It is answered by `Ended` or `Failed`. After a server has answered `Ended` for a
  `Proc`, later waits on it are answered by the caller with the same value and never queued.
- **Released.** Dropping a served `Proc` queues `Released(id, Whole)`, and dropping a pipe
  half queues `Released(id, Read)` or `Released(id, Write)`. Each id's release notice is
  reserved when the reply issues it.
- **Waits after issue.** Every served operation, `spawn` included, waits after issue until its
  server replies ([CONC-7](tasks.md#conc-7-cancellation-points)). A served spawn that its
  caller's cancellation settles first gives `Aborted`, its reply returns `Error(Withdrawn)`
  and issues no ids. A server commits a simulated child only after its reply returns `Ok(())`.

A served child models exit, signal responses, bounded pipe output and the input it expects,
on the caller's virtual `Clock`. A served Exec's results are computed by the run, so they are
not inputs of CAP-8.

Laws:

- a fixed client charges the same against `provide.exec` and against the host, over a sweep
  of limits and step bounds (MEM-24);
- a served spawn withdrawn by cancellation, or a served wait or pipe read withdrawn by its
  deadline, issues no ids and makes no staged bytes visible;
- end of input follows a closed stdin pipe;
- a sealed port answers queued waits with `Other(0)` and pipe I/O with `Reset`.
