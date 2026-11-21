# Recording and replay

A recording is a quick way to keep a trace of one execution for debugging and testing,
without altering the code. The launcher and the test runner make it, and replay is a tool
run against it. No program operation takes, returns or mentions a recording
([CAP-9](spec/capabilities.md#cap-9-recording-is-outside-the-program)), so this page is the
tools' contract, not part of the language.

A recording is not a way to make durable work. It is never synced, nothing resumes from it,
and a crash ends it where it falls. Work that must survive a crash without repeating an
outside effect goes through the store: wrap each such effect in the store's step library
([runner](../core/std/api/runner.md#store-triggers)), whose commits are the one durability
contract ([store](../core/std/api/store.md)).

## Recording builds

A **release build** is every executable built without a recording option: `hirsel build`,
`hirsel run` and `hirsel test`, debug or `--release`. It links the runtime without
recording machinery, so it cannot record, and nothing in it tests whether recording is on.

A **recording build** is the same program, for the same target and engine at the same
optimisation, with the charge and step policies of the build it accompanies, linked with
the recording runtime. These commands make one:

| command | what it does |
| --- | --- |
| `hirsel build app --recording -o OUT` | builds a recording executable; it records only when started with `HIRSEL_RECORD` |
| `hirsel run app --record PATH [grants] [-- arguments...]` | builds the recording build and runs it with recording on, into PATH |
| `hirsel replay app --tape PATH` | builds the recording build and replays PATH with it |
| `hirsel test app --limit N --replay` | runs each selected test in the recording build, recording on, then replays it ([Testing with recordings](#testing-with-recordings)) |

`--release`, `--backend` and `--target` select the engine, optimisation and target of a
recording build as they do for a release build.

A recording executable started with the environment variable `HIRSEL_RECORD=PATH` records
into PATH. The runtime reads the variable and removes it from the environment before the run
starts, so no grant observes it. Started without it, the executable runs with recording off.
A release executable never reads it.

**Same observations.** Given the same values at the replay point
([CAP-8](spec/capabilities.md#cap-8-the-replay-point)) in the same order, a release build,
a recording build with recording off and the same build with recording on make the same
observations ([SEM-8](spec/semantics.md#sem-8-determinism)). Recording runs outside the
program's accounting. It charges no limit, takes no step, adds no event, poll or provider
operation, holds no share of a program value, acts on no program handle and changes no
delivery.

Real elapsed time is not an observation. Recording adds work, so in a live run it can change
which of the allowed orders and clock readings occur. A recording build reproduces a
language-level fault of a release run when that run's inputs recur. It cannot recover an
earlier release run's inputs, and it does not reproduce a fault in the release build's
machine code, native stack layout or host memory pressure.

## What a recording holds

A recording holds the values the run receives at the replay point, in the run's order:

- the entry's arguments: the grant snapshot, the argument vector, the root limit and the
  launch metadata ([launch](spec/launch.md)). A release build and a recording build are
  different executables, so the executable digest a recording holds is the recording
  build's;
- each provider operation's issue: its operation, its object and its canonical arguments;
- each delivery: its result, error, bytes or committed count;
- each scheduler choice ([CONC-10](spec/tasks.md#conc-10-event-order));
- the run's outcome: the exit status it returned, or the causes it failed or was cancelled
  with.

It holds nothing the run computes: parsing, encoding, digests, charges, steps and the
results of events are recomputed by the replay from those inputs.

A recording covers the entry the launcher or the test runner selected and the work that
entry owns. Today the selected entry is `main`, or one test function under `hirsel test`,
and every task the run starts belongs to it.

**Canonical arguments.** An issue's arguments of 16 bytes or fewer are stored literally,
and longer ones as their xxh3-64 digest.

**Sockets.** The run checks each LISTEN and CONNECT against its source Net rules, and a
recording records a denied outcome as the delivery the run received. It records the bound
LISTEN address and each ACCEPT peer address. It checks socket arguments at issue and stores
each delivered receive's bytes in their original fragment sizes. Each send's issue holds
both offered buffers and the absolute deadline, and its delivery how many bytes the host
committed. A final readiness attempt at a deadline can deliver a successful receive,
including its bytes.

**Name resolution.** Each `RESOLVE` issue holds the Net ordinal, the name, the spare
capacity and the deadline. Its delivery holds the addresses in the order delivered, or the
error. The header's grant snapshot records a `Resolve` rule with direction byte 3 and a
`u16` count of patterns, each a length byte followed by its bytes, as a `Tls` rule's.

**Standard input and environment.** Each `stdin.read` issue holds the spare capacity of
its buffer and its deadline. Its delivery holds the bytes in their original fragment
sizes, or `Aborted`, `TimedOut` or the host error. An Env grant is recorded in the grant
snapshot with its patterns and the launcher's snapshot of the covered names and values.
`env.get`, `env.len` and `env.name` read that snapshot and add no frames. A replay
supplies both from the recording and reads neither the host's standard input nor its
environment.

**TLS sessions.** A TLS stream is recorded through the `Net` grant it runs over.

| operation | at issue (checked on replay) | at delivery (supplied on replay) |
|---|---|---|
| `TLS_CONNECT` | socket ordinal; canonical args = `name`, trust (`System`, or the SHA-256 of the roots bundle), the ALPN entries in order, `until` | the outcome. On success the new stream ordinal, the negotiated protocol and the 32-byte peer digest. |
| `RECV_BOUNDED` on a TLS half | as for a socket | the decrypted plaintext, in its original fragments |
| `SEND_PAIR` on a TLS half | as for a socket: both offered plaintext buffers and the deadline | the committed count |
| `SEND_SECRET` | half ordinal; canonical args = `before`, the secret's designation, `after`, `until` | the outcome, and the committed count on failure |

A recording **never** holds ciphertext, handshake records, keys, client or server randoms,
the certificate chain, the roots bundle, or the provider's verification time.

**Secret designations.** A granted secret is a grant. It takes a `GRANT` frame and an object
ordinal in the order of the run's grants, like every other grant. The header's grant
snapshot keeps the parameter name, the `to` patterns and the source label
(`env:OPENAI_API_KEY`), but not the bytes. A sealed secret is designated by the order of its
seal among the run's seals, together with its `to` patterns. The canonical arguments of
`SEND_SECRET` hold the designation, never the bytes or any function of them, because an
unkeyed digest of a short secret could be reversed by trying candidate values.

## Recording failures and truncated recordings

A recording is written as the run goes and never synced. When its storage fails, the
recording ends at its last complete frame and the run goes on unchanged. After the run
ends, the launcher writes `record: truncated at N: REASON` to its own standard error,
never to a stream the program holds, and leaves the run's exit status as the program set
it. A crash or a kill also leaves the recording ending at its last complete frame, with no
report.

A replay of a truncated recording replays its prefix and ends with the verdict `Truncated`
at the first missing position.

## Replay

`hirsel replay app --tape PATH` builds the recording build of `app` and runs it against
PATH with no host provider. It takes the arguments, the root limit and the grant snapshot
from the recording and accepts no grants. It checks each issue against the recording before
any effect, supplies each delivery from the recording, and never repeats a recorded host
effect. It installs no signal handler, reads no `Secret` source and opens no network,
roots or key. Replayed sockets, TLS halves and secrets are local designations: splitting or
dropping one changes only local ownership, sends no `close_notify` and reaches no host.
A replayed `Net` grant starts with no live rules, and its narrowed rule lists and their
`Capacity` charges are computed from the snapshot and each parent's intersections. Launch
metadata is the recorded value, never the digest of the replaying executable.

**Admission.** Before any program code runs, a replay refuses a recording whose entry
identity, entry code revision ([PROG-9](spec/programs.md#prog-9-code-descriptors)),
argument snapshot, root limit or charge policy differs from the replaying build's, and one
whose header is damaged. The code revision covers the reachable checked code, not the engine
or the CPU's vector level, so a recording replays on either engine and on any CPU the build
supports. It exits 2 with `replay: refused: REASON`.

**Across revisions.** `hirsel replay app --tape PATH --diagnose` admits a recording of
another code revision and still refuses the other differences. It writes
`replay: revision differs` first, and reports the first divergent issue at its source
position. It always replays and never continues with live effects.

**Verdicts.** A replay ends with one verdict:

| verdict | meaning |
| --- | --- |
| `Sealed` | every issue matched, every recorded frame was used and the outcome equals the recorded one |
| `Diverged(Op)`, `Diverged(Object)`, `Diverged(Arg)` | an issue's operation, its object or its canonical arguments differ from the recorded issue |
| `Diverged(Missing)`, `Diverged(Extra)` | the run ended with recorded frames unused, or issued past the recording's end |
| `Diverged(Outcome)` | the run's outcome differs from the recorded one |
| `Diverged(Schedule)` | a recorded scheduler choice cannot be taken at its position |
| `Truncated` | the recording ends before the run does |
| `Damaged`, `Corrupt` | a block fails its check, or a frame cannot be decoded |

A verdict other than `Sealed` ends the replay at the position where it is found: the
divergent issue has no effect and is not delivered, and no further program code, cleanup or
finalizer runs. The program observes nothing of it. The replay writes one line,
`replay: VERDICT at N`, where N is the position of the frame it was checked against, counted
after the header, and with `--diagnose` the source position of the divergent issue too. It
then exits 1. A `Sealed` replay exits 0.

## Credentials

A recording holds what servers sent to the run, exactly as received, so a recording of a
run that used a `Net` grant for a TLS session, sent through a `Secret`, or received minted
tokens, `Authorization` headers, `Random` bytes or environment values holds credentials. Every recording is
treated as **credential-bearing**:

- its file is created readable only by its owner, at a path that does not exist yet;
- `hirsel tape show PATH --include-credentials` writes its header and stored frames to
  standard output, and nothing exports a recording without that flag;
- it is never redacted in place, since a replay needs its bytes exactly.

Where cleartext can be in a recording:

| bytes | in the recording? | why |
|---|---|---|
| bytes supplied by a `Secret` grant | never via the grant or send record | Replay uses the designation. |
| a sealed secret | only where the bytes were already there, as the received plaintext they were computed from | Replay recomputes the same bytes from those deliveries, so the seal adds nothing. |
| received plaintext (response bodies, cookies, minted tokens) | always, exactly as received | Replay delivers these bytes to the program, so it needs them exactly. |
| sent plaintext | as the canonical-arguments rule stores it: 16 bytes or fewer literally, otherwise a digest | Replay only checks sends. |

A replay gets the same results without a secret's bytes. The launcher does not read the
secret's source; the replaying run gets a designation with no bytes behind it. No operation
reads a secret's bytes, so the program computes the same values, and the deliveries that
depend on the secret, the server's replies, come from the recording. `SEND_SECRET` on
replay checks the designation and the digests of `before` and `after`, and delivers the
recorded outcome.

## Format

A recording file is a **tape**, in tape format 4, which `crates/hirsel-tape-format` owns.
The format is not part of the language and a replay reads only tapes its own build admits,
so no reader ever sees another build's layout.

- The header has no mode byte. Its grant snapshot stores the run's root Net rules in order
  and with duplicates; each address rule occupies 23 bytes.
- The `Tls` rule record holds the direction byte 2; the trust byte, followed by 32 bytes of
  roots digest when the trust is `Roots`; the port interval; and a `u16` count of patterns,
  each a length byte followed by its bytes.
- A `Secret` grant record holds the parameter name, the source label and the patterns, all
  length-prefixed.
- REPEAT stores periodic CHOOSE frames, and a replay expands them to their original
  positions. `tape show` prints REPEAT once, at the position of the first CHOOSE it
  represents.

## Testing with recordings

A test written in Hirsel makes ordinary assertions and never starts a replay. `hirsel test
app --limit N --replay` adds the replay check to each selected test: the runner runs the
test live in the recording build with recording on and requires its assertions to pass, then
replays that recording and requires `Sealed`. A test fails on a failed assertion, a verdict
other than `Sealed`, or a truncated recording. `--expect-verdict VERDICT` makes a selected
test pass only when its replay ends with that verdict.

A test that wants a fake clock, a scripted directory or a failing service does not use a
recording. It runs against a controlled provider that the run's configuration chooses
([CAP-5](spec/capabilities.md#cap-5-provider-operations)).

## Laws

| law | bug | oracle and setup |
|---|---|---|
| **RL1** recording changes nothing | recording machinery that adds a charge, a step, a poll or a second read, or leaks into a release build | For a corpus of small programs with scripted providers and ordered senders, run the release build, the recording build with recording off and the same build with recording on, on both engines. Their outputs, outcomes, cause sites and chains, `Capacity` refusals, step counts and provider call logs are equal. The release executable defines no symbol of the recording runtime. |
| **RL2** a divergence ends the replay at its position | a replay that continues past a mismatch, or reports it to the program | Record a run, then replay it with the program edited to (a) drop a handle at another point, (b) omit its last send and (c) send other bytes, each still returning the recorded result. Each replay ends `Diverged` at the hand-counted frame position, exits 1, and its scripted provider records zero calls. |
| **RL3** replay is offline | replay reaching the host or needing keys | Record an HTTPS GET against the loopback server. Replay it with a refuse-all inner provider, the server stopped, and the roots file removed. The response bytes, the protocol and the peer digest are equal to the recorded run's, and the inner provider records zero calls. |
| **RL4** replay without the secret | replay reading the secret's source | Replay TL10's recording ([TLS](spec/tls.md#laws)) with the environment variable unset. The run starts. Its outputs equal the recorded run's, byte for byte. The inner provider records zero calls. |
| **RL5** a secret never reaches a recording | a secret's bytes in a frame or a digest | Record TL10's run and search the recording for the value, its base64 and its raw bytes: there are no hits. |
