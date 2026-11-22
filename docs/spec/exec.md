# Process creation

A program starts a native process only through an `Exec` capability. An `Exec` grant names
**launch profiles** that the operator approved when starting the run. Each profile pins an
executable image by its digest and lists the argument forms, environment and working
directories a child may receive. The program supplies literal argument bytes and owns the
child through a `Proc` handle, with optional pipes on the child's standard streams.

The rules are EXEC-1 to EXEC-8. The core declarations and source example are checked.
The laws describe the required runtime behavior.

## Trust

### EXEC-1: An Exec grant confers what its programs can do, and Hirsel does not confine them

A child process is not contained. An `Exec` grant gives the run whatever each named program
can do with the argument forms, environment and working directories its profile allows.
For a general program this is the whole authority of the OS user the run runs as. Hirsel
promises exactly these things about a child, and nothing else:

1. **Image.** The main image that runs is the sealed copy whose SHA-256 the profile pins
   (EXEC-4). Its loader, its shared libraries and any script it is given as an argument are
   host files outside the pin.
2. **Inputs.** The child starts with exactly the argv, environment, working directory and
   descriptors 0, 1 and 2 that EXEC-3 derives from the profile and the call. No other
   descriptor of the run survives into it.
3. **Run memory.** No child gains the access that tracing gates to the run's private memory
   and descriptor table: it cannot attach to the run, read or write that memory, or take
   those descriptors (EXEC-7). The run holds no OS privilege and stays non-dumpable, and
   each child starts with no new privileges, so no child holds the host's tracing privilege
   over the run. Public process metadata (the run's command line and status), memory that
   the run shares with a file or object the user can open, and the child's own executable
   stay readable by the child. A child that asks an outside service holding privilege (a
   service manager, `sudo`, a remote shell) to act for it is outside this promise. Where the
   run's user can reach such a service, it is trusted as the child is.
4. **Direct child.** Dropping a live `Proc` sends its direct child SIGKILL, and an orderly
   run reaps it before it exits (EXEC-6). If the host kills the run, the direct child is
   killed only on a best-effort basis. Nothing is promised about the processes a child
   starts: they can outlive the `Proc`, and they can outlive the run.

**Trust boundary.** An approved profile, together with every program and service it invokes,
is trusted with everything the run's OS user can reach. That includes the run's files, its
recording and every other process of that user. A child cannot itself reach run memory
(promise 3), but it can change anything the user can write, including a recording of this run or an
earlier one. What a child does reaches the program only through this chapter's operations
and through host state that later provider operations read, and each of these is a delivery
at the replay point ([CAP-8](capabilities.md#cap-8-the-replay-point)). EXEC-8 states what a
replay of such a run certifies.

A child's effects on the host are bounded only by the OS. The capability bounds of the run
bind the run's own code and not its children:

- a child may read and write files outside every granted `Dir`;
- it may reach the network outside every `Net` rule;
- it may read the source of a granted `Secret` ([TLS-5](tls.md#tls-5-secrets));
- it may signal other processes of the same user, including the run. A run granted
  `Process` sees a first SIGTERM or SIGINT through `host.stopped`
  ([CAP-5](capabilities.md#cap-5-provider-operations)), and a second one ends
  it at once. A run without `Process` takes the host's disposition for these signals, which
  by default ends it. A run that a signal ends, SIGKILL included, ends as environmental
  termination ([FAIL-15](failure.md#fail-15-run-stops)). A run that ends this way leaves its
  recording ending at its last complete frame ([recording](../recording.md#recording-failures-and-truncated-recordings)).

The program can direct a child through its arguments and its stdin. So a run granted `Exec`
for a general program has, through that program, the authority of its OS user. Argument forms,
environment rules and cwd labels bound what reaches the image. They do not bound what the
image does with it.

**The operator.** An `Exec` grant is explicit at launch. It is given on the command that
starts the run: `--grant 'Exec=FILE NAME[,NAME]'` for `hirsel run`, `hirsel test` and
`hirsel deploy`. It is never given only by a package's configuration or a recipe. `FILE` is
resolved against the launch's working directory, as a `Dir` path is
([launch](launch.md#release-recipe-and-host-assembly)), and is read on the host that starts
the run. Every grant has a **policy digest** (EXEC-2), and the build that the run launches
carries it in its `Exec` binding ([launch](launch.md#release-recipe-and-host-assembly)).

**One rule for every launch.** A process that starts a run with an `Exec` grant loads the
policy from `FILE` once, computes its digest, and compares it with the binding of the build
it launches. It then builds the grant, and every sealed image, from that same parsed policy,
never from a second read of `FILE`. Where one process checks and another launches, the
launching process loads and compares the policy itself. A mismatch refuses that launch
before any process runs with the grant. For `hirsel run`, the binding is the digest computed
when the run's build was made, so a profile file edited between the build and the launch
refuses the run. The rest follows
[FFI-4](foreign.md#ffi-4-the-foreign-capability)'s rules for `Foreign=D` with `Exec=FILE NAME`
in its place:

- `hirsel test` grants a test's configured `"Exec=FILE NAME"` only when its own command line
  holds the same string, and the policy that file gives there has the digest the test build
  was given. Otherwise that test is not started and is reported failed with
  `not granted: Exec[NAME]: pass --grant 'Exec=FILE NAME' to hirsel test`, and the other tests
  run.
- `hirsel deploy` refuses a recipe with an `Exec` binding before admission unless the request
  holds the same `Exec=FILE NAME` string for that binding. It then loads the policy on the
  host and refuses the attempt, before any child of the attempt runs, unless the policy's
  digest equals the binding's. Preview, prepare and serving each load the policy again and
  refuse to start on any other digest. So a change to the policy after approval stops the
  attempt, and changed contents need a new approval. An edit that leaves the policy the
  same, such as a comment, changes no digest and stops nothing.

The digest binds the policy's contents, including each label's directory *path*. It does not
identify the directory found at that path, or an image's interpreter and libraries, which
are resolved at each exec (EXEC-4).

Before any process runs with the grant, the launcher writes one line per profile to stderr
(for `hirsel deploy`, to the deploy's output):
`exec: NAME: image PATH, sha256 E, interpreter I, policy P`. `I` is the `PT_INTERP` path read
from the sealed image, or `none` for a static image. `P` is the grant's policy digest. The
line shows no environment value. What Hirsel enforces as approval is the explicit grant on
the launching command, the match between the loaded policy and the build's binding, and
this emitted record. It cannot establish that anyone read the record.

A contained child, confined to its profile's files, network and descendants by the host, is a
separate later design ([reserved](reserved.md)). The host mechanisms available to an
unprivileged process do not enforce the full [NET-6](net.md#net-6-network-rules) policy on a
native child: Landlock filters TCP ports but no address prefix, and this host class
restricts unprivileged user namespaces. A profile that claimed such a bound on a host that
cannot enforce it would be unsound, so no profile claims one.

## Profiles and authority

### EXEC-2: Launch profiles and narrowing

`Exec` is a core capability. It is plain, `shareable` and `send`, and 8 bytes, and it
designates a set of launch profiles. A profile has these parts:

- a **name**;
- an absolute image path and its pinned SHA-256;
- `argv0`;
- a list of **argument forms**;
- environment rules;
- a set of **cwd labels**, each mapping to an absolute host directory.

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
further arguments. A literal token cannot be spelled `{any}` or `{rest}`. The empty form `[]`
admits only the empty argument list. Environment rules come in two kinds. A **fixed** key
(`LANG`) is always passed with its value and cannot be overridden. A **caller-settable** key,
marked `{any}`, takes the caller's value, or is absent when the caller gives none. Keys are
non-empty and hold no `=` or NUL. `argv0`, every literal token and every fixed value hold no
NUL. Every profile has at least one cwd label.

The launcher reads the profile file before the run starts. A missing file, a malformed or
non-absolute path, a value with a NUL, a malformed profile, an unknown name, or an image that
EXEC-4 refuses makes the run not start ([CAP-4](capabilities.md#cap-4-entry-grants)), and
its one line names the profile and the cause. A script runs through its interpreter's
profile, with the script path as a literal argument, and that path is outside the pin.

The **policy digest** of a grant is the SHA-256 of `"hirsel.exec-policy.v1"` followed by the
granted profiles in bytewise order of name. Each profile is encoded as its name, image path,
pin, `argv0`, forms, fixed keys with their values, caller-settable keys and labels with their
directories, sorted where they are sets. Every variable field carries a `u32` little-endian
length, and every list carries a `u32` little-endian count. The count of granted profiles
follows the domain string. The pin is 32 raw bytes. Each form token starts with a byte
(`0` for a literal, `1` for `Any`, `2` for `Rest`). Only a literal carries a length and
bytes. The digest covers everything a spawn can depend on. Printing it in place of the
fixed values keeps those values off the approval line.

```hirsel core
pub opaque type Exec
pub type ArgToken = Lit(Bytes) | Any | Rest
pub type ExecRule = Programs(names: String) | Form(program: String, form: List[ArgToken]) | EnvKeys(program: String, keys: String) | Dirs(program: String, labels: String)

pub fn narrow(e: Exec, r: ExecRule) -> Result[Exec, RuleError]
```

`exec.narrow` intersects each profile with `r`. Names, keys and labels are comma-separated.
A rule changes only the profile it names, and every other profile is kept unchanged. The one
exception is `Programs`, which keeps exactly the listed profiles.

- `Form` replaces the named profile's forms with their intersections with `form`, and drops
  each intersection that admits no argument list.
- `EnvKeys` intersects the named profile's caller-settable keys with the listed keys. Every
  fixed key and its value is kept unchanged, so a narrowed launch can never lose a value the
  operator fixed.
- `Dirs` keeps the named profile's listed labels.

**Form intersection.** A form is a required prefix of literal and `Any` tokens and an optional
trailing `Rest`. It admits argument lists of exactly its prefix's length, or, with `Rest`, of
at least that length. Two forms `A` and `B` intersect in three steps:

1. Intersect the two sets of admitted lengths. If the result is empty, the intersection is
   empty.
2. The result's prefix length is the larger of the two prefix lengths. The result has `Rest`
   only when both forms have it, since only then is its length set unbounded.
3. At each prefix position, each form contributes its token there, or `Any` where that
   position falls in its `Rest`. Equal literals give that literal, unequal literals make the
   intersection empty, a literal and `Any` give the literal, and `Any` and `Any` give `Any`.

So `[a, Rest] ∩ [a, b, c]` is `[a, b, c]`, `[a] ∩ [a, b, Rest]` is empty, `[Any] ∩ [Any]` is
`[Any]`, and `[] ∩ [Rest]` is `[]`, which admits only the empty argument list.

A profile left with no form or no label is dropped. An empty list is a valid narrowing:
`Programs` with no names keeps no profile, `EnvKeys` with no keys leaves the profile only its
fixed keys, and `Dirs` with no labels drops the profile. A `Rest` anywhere but last, a literal
with a NUL, or a list with an empty or repeated name gives `Invalid` and changes nothing.

Rule storage is admitted at the narrowing and stays charged to its payer until the run ends
([NET-6](net.md#net-6-network-rules)). Its logical size is 32, plus for each kept profile 8
plus its name's bytes, plus per form 8 plus 8 per token plus each literal's bytes, plus per
caller-settable key and per label 8 plus its bytes, plus 8 per fixed key, rounded up to 8.
The fixed values, paths and sealed images belong to the launch-time grant. Every narrowing
shares them, and no limit pays for them.

A served Exec (EXEC-8) starts universal, so its narrowings keep parts that no host grant
has. Its logical grant is a finite list of named profiles plus a **remainder**, which is
either absent or "every other name, unconstrained". In a named profile, the forms, the
caller-settable keys and the labels may each be universal, and a profile of a served grant
has no fixed key. So `Form("git", f)` on a universal grant gives a named profile `git`
with forms `[f]` and universal keys and labels, plus the universal remainder, and
`Programs` leaves no remainder. In the size above, a present remainder adds 8, and each
universal forms, keys or labels part counts 8 in place of its list. A host grant never has
a remainder or a universal part. For example, `Form("git", [Lit("status")])` on a universal
grant has size 32 + (8 + 3) + (8 + 8 + 6) + 8 + 8 + 8 = 89, rounded up to 96: the profile
`git`, its one form, its universal keys, its universal labels and the remainder.

When a narrowing drops a named profile and keeps the universal remainder, it retains the name
with empty forms, keys and labels. The remainder excludes that name. This entry adds only
8 plus the name's bytes to the formula. For example, `Dirs("git", "")` on a universal grant
keeps every other name and denies `git`. Its size is 32 + 8 + (8 + 3) = 51, rounded up to 56.

The size is
a function of the logical grant alone, so equal logical grants charge the same on the host,
on a served Exec and in a replay.

Laws:

- for every profile `P`, rule `R` and launch `L`, the narrowed profile admits `L` exactly
  when `P` admits `L` and `R` allows it ([CAP-7](capabilities.md#cap-7-attenuation));
- every successful spawn through a narrowed grant gives the child an image, argv, envp, cwd
  and stdio that its parent grant gives for the same launch;
- the form intersection admits exactly the argument lists that both forms admit, checked
  against the worked examples above and against enumeration over short lists;
- a narrowing charges exactly the size of the logical grant it yields, including a served
  grant's remainder and universal parts, so narrowings that yield equal logical grants
  charge the same on the host and on a served Exec, and a replay charges what the run did.

## Spawning

### EXEC-3: Spawn

```hirsel core
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

**Before issue.** `exec.spawn` first makes the checks that the logical grant and the
caller's data decide on their own. Each that fails gives `Denied` with no host effect:

- `program` names a profile of `e`;
- `args` matches one of its forms, and no argument holds a NUL;
- every `env` key is one the profile marks caller-settable, and no key appears twice;
- no value holds a NUL;
- `dir` is one of its labels;
- `path` is empty or a valid path ([core](core.md#contracts-the-signatures-do-not-state));
- each field's length and each list's count fits in a `u32`.

The spawn then admits every result handle's block
([MEM-24](budgets.md#mem-24-logical-charges)). A refused admission raises `Capacity` with no
host effect. So once the host has created a child, its result can always be delivered.

**At the host.** Each failure from here on depends on the host. It is a delivered result,
and a recording supplies it on replay, which consults no host state:

- the provider reserves the supervisor's record for the child in its own storage, outside
  every limit, before it creates the child. A failed reservation gives `Other(errno)`, and
  no child exists. So a created child's drop can always be handled;
- a `path` that does not resolve beneath the label's directory to a directory, through no
  symbolic link and no escape, gives `Denied`;
- a host that cannot create the child, or a child whose setup or exec fails, gives
  `Other(errno)` once that child has been reaped.

The child receives exactly:

- **argv** `argv0` followed by `args`.
- **envp** the profile's fixed values and the caller's values for caller-settable keys.
  Nothing is inherited from the run's environment, and the order is bytewise by key.
- **cwd** the directory that resolution found.
- **descriptors** 0, 1 and 2: `/dev/null` for `Discard`, or a new pipe for `Piped`. No
  other descriptor survives exec.
- **signals** every catchable signal at its default disposition, whatever the run had
  ignored, and an empty signal mask.
- **privileges** no-new-privileges, so exec gains no set-user-ID or file capability, and a
  parent-death signal of SIGKILL, armed during setup (EXEC-6).
- **search** no `PATH` search and no shell.

**Child setup.** The host creates the child with a pidfd in one step (EXEC-5), so the
child's identity is pinned before anything can reap it. The child then works only in
descriptor state of its own:

1. It places its standard descriptors on 0, 1 and 2. Every descriptor the setup still
   needs that sits on 0, 1 or 2 is first moved above 2: each standard source, the cwd, the
   sealed image and the error channel. Placing a descriptor on an occupied number would
   close that number's occupant.
2. It enters the resolved cwd by its descriptor.
3. It closes every descriptor except 0, 1, 2, the sealed image and a close-on-exec error
   channel.
4. It resets its signal dispositions and mask, and sets no-new-privileges. It arms the
   parent-death signal, then checks that its parent is still the run, and exits without
   executing the image if the run has already died.
5. It executes the sealed image by descriptor.

A failure at any step writes the step and `errno` to the error channel, and the child exits.
The parent reaps that child before it delivers `Other(errno)`. When the channel closes with
no report, the child is returned as a `Proc`. That close shows only that no setup step
reported a failure. It does not show that the image reached its own code. `wait` reports how
the child actually ended.

For each `Piped` stream the result holds the run's end, a `WriteHalf` for stdin and a
`ReadHalf` for stdout and stderr, and `None` for each `Discard`. These are the halves of
[NET-7](net.md#net-7-owned-connections): `host.send_pair` and `host.recv_bounded` move their
bytes with the same partial commits, deadlines and cancellation, and dropping a `WriteHalf`
closes the pipe, so the child reads end of input. A write to a pipe whose reader has closed
gives `Reset`. `net.join` refuses a pipe's halves (NET-7 check 1), so no pipe reaches TLS.
`Proc` is a mechanical resource of logical size 8, `send`, move-only and never `shareable`.

`spawn` is a cancellation point at its issue only, and the host provider completes it at its
issue ([CONC-7](tasks.md#conc-7-cancellation-points)). `TimedOut` and `Aborted` come only
from a served Exec (EXEC-8).

```hirsel
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

Law: a child sees exactly the argv, envp, cwd, descriptors 0 to 2, signal state and
privileges listed above. This holds when the run itself has descriptors open on 0 to 2 or
closed there, and when it has signals ignored or blocked. A failure at each setup step is
delivered as `Other` only after that child has been reaped.

### EXEC-4: The image is the pinned bytes

At launch, for each granted profile, the launcher:

1. copies the image path's bytes into a fresh private sealable anonymous file;
2. seals that file against every write, shrink and grow, and checks that all three seals
   hold, so no writer, shared or not, can change it afterwards;
3. on those same sealed bytes, checks the SHA-256 against the pin, checks that they are a
   native executable of the host's architecture with no `#!` line, and reads the
   interpreter path that the approval line shows;
4. keeps the sealed file for the run's spawns.

If any step fails, the run does not start, and its line names the profile and the step. A
seal that permits only future writes is not enough: a writer that already maps the file
could still change it.

Every spawn of that profile executes that sealed copy by descriptor (`execveat` with
`AT_EMPTY_PATH`). Renaming, replacing or rewriting the image file after launch, by any
process, changes nothing that runs. If the host refuses to execute the sealed copy, for
example with `ETXTBSY`, that spawn fails with `Other(errno)`; this affects only
availability. The pin covers the main image's bytes alone. A dynamically linked image's
interpreter and shared libraries are host files resolved at each exec and are not pinned.
The sealed copy is host memory outside every limit and lives for the run.

Law: the main image file that each exec loads is the sealed copy, and its bytes at that
exec equal the bytes the digest was checked on, including when a process writes or
truncates the image path at any point from step 1 on. The law is stated at the exec: after
entry, the image may change its own private pages, as any native program may.

## Owning a child

### EXEC-5: Wait and signal

```hirsel core
pub type Termination = Exited(u8) | Signalled(u8)
pub type Signal = SigInt | SigTerm | SigKill

pub fn wait(mut p: Proc, until: Until) -> Result[Termination, ExecError] waits
pub fn signal(mut p: Proc, s: Signal) -> Result[(), ExecError] waits
```

A `Proc` holds the pidfd its child was created with. The run never reaps by process ID or by
wildcard, and never leaves SIGCHLD ignored, so no child of the run is reaped behind a
`Proc`'s back. A `Proc` therefore names one process for its whole life.

`exec.wait` waits until the child has ended and has been reaped through its pidfd. It returns
`Exited(code)` for an exit and `Signalled(number)` for termination by a signal. A nonzero
code is a successful wait. The termination is then cached in the `Proc`: every later `wait`
returns it at once with no host call. `wait` waits after issue, and it is a cancellation
point at issue and while it waits. Cancellation there gives `Aborted`, and a passed `until`
gives `TimedOut`. Either way the child is unchanged and `p` still owns it.

`exec.signal` sends SIGINT, SIGTERM or SIGKILL to the child through its pidfd, never by
process ID, so it never reaches another process. `Ok(())` says the request was made. A
`Proc` whose termination is cached sends nothing and returns `Ok(())` at once. `signal`
completes at its issue. There is no operation on arbitrary process IDs, process groups or a
detached child. A lifetime limit is library code: signal `SigTerm`, wait with a deadline,
then signal `SigKill` and wait.

Laws:

- a `Proc` reaps its own child exactly once, and every later wait returns the same value;
- no signal from `signal`, from a drop or from the supervisor reaches a process other than
  its child, even when the child has been reaped and its process ID reused.

### EXEC-6: Dropping a Proc kills its child

Dropping a `Proc` whose child has not been reaped sends that child SIGKILL through its pidfd
and hands the pidfd to the run's **supervisor**, which reaps it. The finalizer never waits.
The record the supervisor needs was reserved before the child existed (EXEC-3), so a drop
cannot fail.

**The registry.** The supervisor keeps one registry of every process the run created: each
pending or live `Exec` child, and each of the runtime's own helper processes. A child is
entered under the same lock that serializes its creation, so a supervisor scan never sees a
child of the run that is missing from the registry. Adoption, termination caching and
removal take that lock as well.

**Orphans.** A run granted `Exec` is a child subreaper, so a process orphaned below one of its
children becomes a child of the run. When a child ends, is reaped or is dropped, and when
the run ends, the supervisor lists the run's children. For each one not in the registry it
opens a pidfd, then asks the kernel, through that pidfd, whether the process it pins is a
child of the run that is still unreaped. Since only the run reaps its children, and only
through pidfds (EXEC-5), that process stays the run's child until the supervisor reaps it.
Only after that check passes does the supervisor signal and reap it, through the same pidfd.
A process whose parentage it cannot verify this way is skipped, and a process ID found by a
scan is never signalled. This cleanup is best effort. A listing can miss a child that is
exiting at that moment, processes can be replaced faster than the scans run, and a child
that is itself a subreaper keeps its own orphans. So no completion bound is given, and no
descendant is attributed to a particular `Proc`.

**The run's end.** An orderly end, whether a return, a failure or a run stop that runs
cleanup ([FAIL-15](failure.md#fail-15-run-stops)), kills and reaps every registered `Exec`
child and makes the orphan pass above. Helpers are ended by the runtime that owns them.

**When the host kills the run**, no cleanup runs, and a direct child's death is best effort.
Setup arms a parent-death signal of SIGKILL and then checks that the run is still its parent
(EXEC-3), so a child whose run died before the signal was armed exits without executing the
image. The supervisor creates every child from a thread that does not exit while the run
lives, so the signal is not sent while the run lives. The kernel sends it when the run dies
only if the setting is still armed then. It is not if the image cleared it, or if the child
changed its effective or filesystem user or group ID, which the kernel answers by clearing
the setting. EXEC-7 does not forbid either. Deeper descendants may keep running. Processes
that an outside service starts on a child's request (a user service manager, a scheduler, a
remote shell) are not descendants at all.

Tearing down the whole tree, or promising any child's death after the host kills the run,
would need process containment that the run neither owns nor can escape, with a
verified-empty check before it reports completion: a delegated cgroup, or a PID namespace.
Neither is available to an unprivileged process on every host of the supported class, so
that is part of the contained kind ([reserved](reserved.md)) and not of EXEC-6.

Laws:

- after a drop, in an orderly run, the direct child is reaped before the run exits;
- at an orderly end, every registered `Exec` child is killed and reaped before the run exits;
- an orphan that the supervisor signals was, at that moment, an unreaped child of the run
  outside the registry, as the kernel reported through the pidfd it is signalled through;
- the supervisor never signals or reaps a helper process or a pending spawn.

### EXEC-7: A run granted Exec keeps its children out of its private state

Before any launch resource exists, a run granted `Exec` checks, on the supervisor thread
that will create every child, the credentials that its children will inherit. Its real, effective and saved user IDs must all be nonzero, and its permitted
and effective capability sets must be empty. An empty permitted set implies an empty ambient
set, so all three are empty. User ID 0 is refused in the run's own user namespace, so a root
user remapped in a container is refused too. A capability in the bounding set is not one the
run holds, and it is not checked. A run that fails the check does not start, and its one line
says `Exec requires an unprivileged run`. The run then makes itself non-dumpable.

Credentials belong to each thread, and a child inherits those of the thread that creates
it. The supervisor thread creates every child, and the runtime never changes any thread's
credentials or the run's dumpable setting. So every child starts from the checked
credentials. Native code reached through `Foreign` is trusted to keep this, as it is trusted
with the rest of the native contract ([FFI-1](foreign.md#ffi-1-hirsel-trusts-audited-native-code-and-does-not-contain-it)).

Each child starts with no-new-privileges and a nonzero user ID (EXEC-3), so it holds no
capability in the run's user namespace. A user namespace that a child creates gives it
capabilities only inside that namespace, never in the run's. While the run stays
non-dumpable, which it does until it exits, the kernel lets only a process holding the
tracing privilege in the run's user namespace attach to the run, read or write its memory
through `/proc` or `process_vm` calls, read its descriptor links, or take a descriptor by
`pidfd_getfd`. So no child gains any of these, whatever the host's tracing policy. With
EXEC-3's descriptor rule, this keeps `Secret` bytes held in the run's private memory and
every run descriptor out of the children's reach.

That is all the promise covers. The run's command line, status and other public process
metadata stay readable by every process of the user. Memory the run shares with a file the
user can open is readable through that file. A child can read its own executable: the seal
keeps the image's bytes fixed, and it keeps nothing about them confidential. A run granted
`Exec` writes no core dump. A run without an `Exec` grant does not change these settings.

On Linux the provider needs, in addition to `Dir`'s floor:

- `clone3` with `CLONE_PIDFD` (Linux 5.3);
- `close_range` (Linux 5.9);
- sealable anonymous files;
- no-new-privileges.

A run granted `Exec` probes them once before `main`. Where one is unavailable the run does not
start, and its one line names the cause, `Exec requires clone3 with CLONE_PIDFD and
close_range (Linux 5.9 or later)`.

Laws:

- a run whose real, effective or saved user ID is 0, or whose permitted or effective set
  holds any capability, does not start with an `Exec` grant;
- no child gains attach, memory or descriptor access to a running run granted `Exec`.

## Replay and simulation

### EXEC-8: Exec at the replay point and served Exec

**Recording.** A program that reaches `exec.spawn` may be recorded and replayed. Each effect
a child has on the program arrives as a recorded delivery (EXEC-1), unlike foreign code
([FFI-8](foreign.md#ffi-8-a-program-that-reaches-foreign-code-is-never-recorded-replayed-or-reused)).

- *The grant snapshot* holds each profile's name, its image digest, `argv0`, its forms, its
  environment keys each marked fixed or caller-settable, and its cwd labels. It holds no
  fixed value, image path or label directory.
- *A spawn's issue* holds the `Exec` ordinal and the canonical launch as one argument.
  `program`, `dir` and `path` come first, each as a `u32` length and its bytes. Then come a
  `u32` argument count with each argument as a length and bytes, and a `u32` environment
  count with each key and value as lengths and bytes in bytewise key order. Last come
  `stdin`, `stdout` and `stderr`, one byte each (0 `Discard`, 1 `Piped`). The ordinary rule
  for canonical arguments then stores it literally or as a digest
  ([recording](../recording.md#what-a-recording-holds)).
- *Its delivery* holds the new `Proc` and pipe ordinals, or the error, including the `Denied`
  of a cwd that did not resolve and the `Other(errno)` of a failed supervisor reservation.
  A replay consults no host path and reserves no supervisor record.
- *A wait* holds the `Proc` ordinal and `until` at issue, and the termination or the error at
  delivery. A cached wait makes no host call and is not recorded.
- *A signal* holds the ordinal and the signal at issue, and the outcome at delivery.
- *Pipe I/O* uses the half rows ([recording](../recording.md#what-a-recording-holds)).
- *A release.* Dropping a host-issued `Proc` or pipe half issues the provider's `CLOSE`
  operation with its ordinal and part, as dropping any host handle does, and the recording
  holds it at its position in the run's order. It records only that the run asked for the
  kill or close, never that the child died or read end of input.
- *An impossible delivery*, one that its issue could not have produced, is `Corrupt`. That
  covers a pipe count above the room or the offer, a spawn result without exactly the piped
  ordinals, and an error a host spawn never returns, such as `TimedOut`. The replay finds it
  before it changes any buffer or ownership.

A replay starts no process. It creates local handles, and it checks each `CLOSE` at its
position: a drop that moved diverges there
([RL2](../recording.md#laws)). On a release it updates local ownership only, and it issues
no kill, close or reap.

**What replay certifies.** A replay compares the program against the tape it is given. A
`Sealed` verdict says that this program makes exactly the recorded issues and reaches the
recorded outcome against those deliveries. It does not say which execution produced the
tape, or that the tape is unchanged since it was recorded. Any process of the recording
user, including every child of a run granted `Exec`, can rewrite it. A deployment that needs
recording integrity against a child keeps the tape where the child cannot write it, or does
not record that run.

A tape holds no host root mapping, image path field, native process ID, native descriptor
number or fixed-value field. Logical labels, `argv0`, argument literals, the caller's bytes
and delivered payloads are recorded by the ordinary rules, and any of them can contain such
values: a child may print its cwd, its process ID or a fixed value on stdout. Processes,
pipes and sockets appear on tape as typed run-local ordinals.

**Served Exec.** `provide.exec(bytes n)` makes an `Exec` of virtual construction with its
`Served[ExecOp]` port ([core](core.md#provide)). It permits every program name, form, key and
label through internal universal rules. A universal rule intersected with a concrete rule
gives that concrete rule, and it has no external spelling and no tape tag. `exec.narrow`
narrows it, and EXEC-2 gives the logical grant and the rule-storage charge of each result,
its remainder and universal parts included.

```hirsel core
pub type ExecOp = Spawn(program: u64, args: u64, env: u64, dir: u64, path: u64, stdin: Stdio, stdout: Stdio, stderr: Stdio) | Await(proc: u64) | Signal(proc: u64, signal: Signal) | Recv(pipe: u64, room: u64, max: u64) | Send(pipe: u64, prefix: u64, tail: u64) | Released(id: u64, part: Part)
pub type ExecReply = Done | Failed(ExecError) | IoFailed(NetError) | Count(u64) | Started(proc: u64, stdin: u64, stdout: u64, stderr: u64) | Ended(Termination)

pub fn exec(bytes n: u64) -> (Exec, Served[ExecOp])
pub fn exec_op(c: Call[ExecOp]) -> ExecOp
pub fn exec_reply(r: ExecReply) -> provide.Reply[ExecOp]
```

The served rules are Provide's: a port room, payload fields by declaration order, `fill`,
reply as the commit point, withdrawal, sealing to `Reset` (pipe I/O) or `Other(0)` (other
operations), fresh kind-checked ids issued only by a reply, and cross-provider handles
`Denied`.

- **Spawn's payloads.** `args` is each argument as a `u32` little-endian length followed by
  its bytes. `env` is each key and then its value in the same form, sorted by key.
- **Replies.** Each operation accepts only these replies. Any other reply, id or count fails
  the server with `Fail` before the caller changes:
  - `Spawn`: `Started` or `Failed`;
  - `Await`: `Ended` or `Failed`;
  - `Signal`: `Done` or `Failed`;
  - `Recv` and `Send`: `Count` or `IoFailed`.
- **Started's ids.** A `Started` reply issues the `Proc` id and exactly one pipe id for each
  `Piped` stream, and 0 for each `Discard`. The ids must be fresh and pairwise distinct. The
  server's room for all of them is reserved together, so the reply issues all of them or
  none.
- **Counts.** A `Recv` reply's count must equal the bytes the server staged with `fill`,
  which is at most `min(room, max)`. A `Send` reply's count is at most the bytes offered.
- **Error replies.** A `Failed` or `IoFailed` reply delivers no bytes. It discards whatever
  the server staged, so the caller's buffer is unchanged, and it issues no ids.
- **A terminal Proc.** After a server has answered `Ended` for a `Proc`, later waits on it
  are answered from the caller's cache with that value, and later signals return `Ok(())`
  locally. Neither is queued. A cancellation at the entry of such a wait still follows
  [CONC-7](tasks.md#conc-7-cancellation-points).
- **Released.** Dropping a served `Proc` queues `Released(id, Whole)`, and dropping a pipe
  half queues `Released(id, Read)` or `Released(id, Write)`. Each id's release notice is
  reserved when the reply issues it.
- **Waits after issue.** Every served operation, `spawn` included, waits after issue until its
  server replies ([CONC-7](tasks.md#conc-7-cancellation-points)). A served spawn that its
  caller's cancellation settles first gives `Aborted`. Its reply returns `Error(Withdrawn)`,
  and it issues no ids. A server commits a simulated child only after its reply returns
  `Ok(())`.

A served child models exit, signal responses, bounded pipe output and the input it expects,
on the caller's virtual `Clock`. A served Exec's results are computed by the run, so they are
not inputs of CAP-8.

Laws:

- a fixed client that does not narrow its grant charges the same against `provide.exec`
  and against the host, over a sweep of limits and step bounds (MEM-24). A narrowing of a
  served grant charges EXEC-2's size of the logical grant it yields. The sweep includes the exact admission boundary of a
  spawn with three pipes, and a server one byte short of room for a `Started` reply;
- a served spawn withdrawn by cancellation, or a served wait or pipe read withdrawn by its
  deadline or answered with an error reply, issues no ids and makes no staged bytes visible;
- every reply outside the accepted set above fails the server with the caller unchanged;
- end of input follows a closed stdin pipe;
- a sealed port answers queued waits with `Other(0)` and pipe I/O with `Reset`;
- a replay of a run edited to move a `Proc` drop, or a stdin half's drop, past another issue
  diverges at its `CLOSE`.
