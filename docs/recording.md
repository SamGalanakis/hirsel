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

Recording is an optional switch, off by default. It is independent of the engine, the
target and the optimisation: `--release` and `--backend` choose those, for a build with
recording or without it, and choose nothing about recording. A build without the switch,
every `hirsel build`, `hirsel run` and `hirsel test` that names no recording option, links
the runtime without recording machinery, so it cannot record, and nothing in it tests
whether recording is on.

A **recording build** is the same program, for the same target and engine at the same
optimisation, with the charge and step policies of the build it accompanies, linked with
the recording runtime. These commands make one:

| command | what it does |
| --- | --- |
| `hirsel build app --recording -o OUT` | builds a recording executable; it records only when started with `HIRSEL_RECORD` |
| `hirsel run app --record PATH [grants] [-- arguments...]` | builds the recording build and runs it with recording on, into PATH |
| `hirsel replay app --tape PATH` | builds the recording build and replays PATH with it |
| `hirsel test app --limit N --replay` | runs each selected test in the recording build, recording on, then replays it ([Testing with recordings](#testing-with-recordings)) |
| `hirsel test app --limit N --laws` | observes law outcomes in selected tests ([law table](laws.md#observation)) |

**Law observation** is part of the recording runtime. A
recording executable started with `HIRSEL_LAWS=PATH` writes into PATH the settled outcomes of
outermost `std.prop` and `std.model` engine calls, as [law table](laws.md) says. The observer
captures the outcome on return or, for a failing check, immediately before the engine raises its
own failure. It is read and
removed like `HIRSEL_RECORD`, works with recording on or off, and is under the same-observation
rule below. A build without recording contains none of it.

Version note (2026-11-24, verify-phase-spec-2034): adds planned law observation to the
recording build, so CAP-9 keeps one build switch.

Version note (2026-11-24, verify-observer-2037): defines the observation point for
an engine check that raises a failure.

A program whose entry reaches a foreign function has no recording build. Recording and
replay commands refuse it before building, reading a tape or running native code, as
[FFI-8](spec/foreign.md#ffi-8-a-program-that-reaches-foreign-code-is-never-recorded-replayed-or-reused)
says: `hirsel replay` exits 2 with `replay: refused: FFI-8: …`, and `hirsel test --replay`
fails each foreign-reaching test alone. `hirsel test --laws` runs those tests in the
ordinary build and reports `unobserved (FFI-8)`.

`--release`, `--backend` and `--target` select the engine, optimisation and target of a
recording build as they do for any other build.

A recording executable started with the environment variable `HIRSEL_RECORD=PATH` records
into PATH. The runtime reads the variable and removes it from the environment before the run
starts, so no grant observes it. Started without it, the executable runs with recording off.
An executable built without recording never reads it.

**Same observations.** Given the same values at the replay point
([CAP-8](spec/capabilities.md#cap-8-the-replay-point)) in the same order, a build without
recording, a recording build with recording off and the same build with recording on make the same
observations ([SEM-8](spec/semantics.md#sem-8-determinism)). Recording runs outside the
program's accounting. It charges no limit, takes no step, adds no event, poll or provider
operation, holds no share of a program value, acts on no program handle and changes no
delivery.

Real elapsed time is not an observation. Recording adds work, so in a live run it can change
which of the allowed orders and clock readings occur. A recording build reproduces a
language-level fault of a run without recording when that run's inputs recur. It cannot
recover such a run's inputs, and it does not reproduce a fault in that build's machine
code, native stack layout or host memory pressure.

## What a recording holds

A recording holds the values the run receives at the replay point, in the run's order:

- the entry's arguments: the grant snapshot, the argument vector, the root limit and the
  launch metadata ([launch](spec/launch.md)). A build without recording and a recording
  build are different executables, so the executable digest a recording holds is the recording
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

**Spare capacity.** An issue that reads into a buffer (a listing read, a positioned read,
`RESOLVE`, `DGRAM_RECV`, `stdin.read`, a credential fetch) records its buffer's spare
capacity, but a replay does not compare the replaying buffer's spare capacity with the
recorded one. It requires only that the recorded delivery fits: the bytes, records or
addresses delivered are at most the replaying buffer's spare capacity, and a replay whose
buffer is too small for the recorded delivery ends `Diverged` at that issue (RL2). So a
buffer that grew or shrank between builds, by a capacity rounding or another growth
policy, replays as long as each delivery still fits. A delivery that contradicts the
recorded room, such as a datagram whose length and truncation flag disagree with the
recorded spare capacity, is still `Corrupt`, because the room it is checked against is the
recorded one.

**Directory and positioned file operations.** Narrowing and opening a listing
deliver new object ordinals. Each listing read checks the handle ordinal and
that the records fit its spare capacity, then supplies whole records in the recorded order. Stat
operations supply kind, size and signed modification nanoseconds. Positioned
I/O checks the file ordinal, offset, and input bytes, and for a read that the bytes fit
its spare capacity, then supplies the read bytes or the committed write count.

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

**Child processes and Unix sockets.** An `Exec` grant is recorded in the grant
snapshot with each profile's name, image digest, `argv0`, argument forms, environment keys
each marked fixed or caller-settable, and cwd labels. A `Unix` rule is recorded with its
direction, root label and path. The provider metadata holds no host root mapping, image path,
fixed value, native process ID or descriptor number. Logical labels, `argv0`, argument
literals, the caller's bytes and delivered payloads follow the ordinary rules, and any of
them may contain such values.

| operation | at issue (checked on replay) | at delivery (supplied on replay) |
|---|---|---|
| `SPAWN` | Exec ordinal; one canonical argument: program, cwd label and path, each with a `u32` length; the argument count and each argument with its length; the environment count and each key and value with lengths, sorted by key; the three stdio modes, one byte each | the new `Proc` ordinal and one ordinal per piped stream, or the error, including `Denied` for a cwd that did not resolve |
| `WAIT` | `Proc` ordinal; `until` | the termination (`Exited` code or `Signalled` number), or the error |
| `SIGNAL` | `Proc` ordinal; the signal | the outcome |
| `RECV_BOUNDED`, `SEND_PAIR` on a pipe half | as for a socket | as for a socket |
| `UNIX_CONNECT`, `UNIX_LISTEN`, `UNIX_ACCEPT` | Net or listener ordinal; root label, path, backlog or `until` | the new ordinal, or the error; `BoundNotListening` only for `UNIX_LISTEN`, and `Corrupt` elsewhere |
| `DGRAM_OPEN`, `DGRAM_BIND`, `DGRAM_CONNECT` | Net or datagram ordinal; root label and path where given | the new ordinal or the outcome |
| `DGRAM_SEND`, `DGRAM_SEND_TO` | datagram ordinal; root label and path for send-to; the packet; `until` | the outcome |
| `DGRAM_RECV` | datagram ordinal; spare capacity; `until` | the appended bytes, the packet's length and the truncation flag, or the error |
| `CLOSE` of a `Proc`, pipe half or Unix socket | the ordinal and part, as for every dropped host handle | the outcome |

A replay starts no process and opens no socket. It matches each `CLOSE` by its ordinal and
part, not by its position among the frames: a handle's drop may come earlier or later than
it did in the recording, because when a value is dropped is not an observation the
program's meaning fixes. Ordinals are allocated in delivery order and never reused, so the
match is exact. A `CLOSE` contributes no scheduler event. A `CLOSE` the recording does not hold for that ordinal and part, or a
recorded `CLOSE` whose handle the replay never drops before the run ends, ends the replay
`Diverged`, the second at the run's end (RL2). It then changes local ownership only: nothing is killed,
closed or unlinked on the host. A delivery that its issue could not have produced, such as a
packet whose length and flag contradict its room, is `Corrupt`
([EXEC-8](spec/exec.md#exec-8-exec-at-the-replay-point-and-served-exec)).

A recording is a file of the recording user. Every child of a run granted `Exec` runs as that
user and can rewrite it, or put another valid tape of the same program in its place. Replay
compares the program against the tape it is given. A `Sealed` verdict does not say which
execution produced that tape.

**Terminal bits.** The `GRANT` frame of a `Stdin`, `Stdout` or `Stderr` grant carries one
byte after its kind: 1 when the stream was a terminal at launch, 0 otherwise
([stdin](../core/std/api/stdin.md#terminals)). `is_terminal` reads it and adds no frames, and
a replay answers from it without probing its own descriptors.

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
| `TLS_CONNECT` | socket ordinal; canonical args = `name`, trust (`System`, or the SHA-256 of the roots bundle), the ALPN entries in order, `until` | the outcome. On success the new stream ordinal, the negotiated protocol, the 32-byte peer digest and the binding value with its length (0 when absent). |
| `RECV_BOUNDED` on a TLS half | as for a socket | the decrypted plaintext, in its original fragments |
| `SEND_PAIR` on a TLS half | as for a socket: both offered plaintext buffers and the deadline | the committed count |
| `SEND_SECRET` | half ordinal; canonical args = `until`, each secret's designation in `secrets` order, `wire`, `marks` | the outcome, and the committed count on failure |
| `SECRET_MAC` | run-owned sink at object 0; modes 0/1 for SHA-256, 7/8 for SHA-384 and 9/10 for SHA-512 (sign/verify). Modes 11/12, 13/14 and 15/16 enforce the same algorithms' JWS key minimums. Canonical args = mode, secret designation and policy, `data` and the input tag for verify. | the 32-, 48- or 64-byte tag, the verification verdict or the JWS `ShortKey` refusal |
| `SECRET_KDF` ([TLS-5](spec/tls.md#tls-5-secrets)) | run-owned sink at object 0; canonical args = parent designation and policy, child policy, then `key_prefix` and `data` for HMAC, or the mode, `salt`, `len` and `info` (HKDF) or `cost` (Argon2id) | the delivery position or Argon2id's delivered `Invalid`, without key bytes |
| `SECRET_MAC` key operations ([TLS-5](spec/tls.md#tls-5-secrets)) | object 0; mode 2 `wrap`, 3 `unwrap`, 4 `sign`; canonical args = mode, the key's designation and policy, then for `wrap` the wrapped secret's designation and policy, for `unwrap` the `uses` text and the blob, for `sign` the algorithm, `Random` identity and `msg`, and the spare of `out` | the outcome; for `wrap` and `sign` on success the blob or signature. `Capacity`'s available bytes are the recorded spare. |
| `SECRET_MAC` AEAD ([TLS-5](spec/tls.md#tls-5-secrets)) | object 0; mode 5 `aead_seal`, 6 `aead_open`; canonical args = mode, key designation and policy, algorithm, nonce, associated data, plaintext or sealed bytes, and output spare | the refusal or the appended bytes, including the authenticated plaintext for open |
| `MEDIA_KEY_SHARE` ([TLS-8](spec/tls.md#tls-8-packet-ciphers-and-key-agreement)) | object 0. Canonical args bind the Random designation, group and public buffer spare. | The delivery position and public key bytes. Replay holds a share ticket with no private key. |
| `MEDIA_P256_KEY` ([TLS-8](spec/tls.md#tls-8-packet-ciphers-and-key-agreement)) | root-owned key generator at object 0. Canonical args bind the Random designation, signing policy and public buffer spare. | The delivery position and 65-byte public point. The Secret’s document stays with the provider. |
| `MEDIA_CAPTURE` ([TLS-8](spec/tls.md#tls-8-packet-ciphers-and-key-agreement)) | Root media outcome at object 0. Canonical args bind the operation, public arguments and writable spare. | A child origin, public output bytes, or a refusal. Replay validates the delivery shape before it writes any bytes. |
| `MEDIA_AGREEMENT` ([TLS-8](spec/tls.md#tls-8-packet-ciphers-and-key-agreement)) | root-owned registry verdict at object 0. Canonical args bind the constructor's public operands and share origin, including for virtual shares. | `Invalid` (0), accepted (1), or `Issued` (2). Replay supplies the verdict without an agreement provider call. |
| `SCRAM` | half ordinal; canonical args = secret designation, `client_first`, `server_first`, `before`, `after`, `until` | the outcome, and the committed count on failure. The new `Scram`'s ordinal on success. |
| `SCRAM_VERIFY` | `Scram` ordinal; canonical args = `server_final` | the verdict |

A recording **never** holds ciphertext, handshake records, keys, client or server randoms,
the certificate chain, the roots bundle, or the provider's verification time. Nor does it
hold any SCRAM value computed from a password: the normalized password, `SaltedPassword`,
`ClientKey`, `StoredKey`, `ServerKey`, the client proof or the expected server signature
([TLS-6](spec/tls.md#tls-6-scram-sha-256)).

**Secret designations.** A granted secret is a grant. It takes a `GRANT` frame and an object
ordinal in the order of the run's grants, like every other grant. The header's grant
snapshot keeps the parameter name, the canonical policy text, and the source label
(`env:OPENAI_API_KEY`), but not the bytes. A sealed secret is designated by the order of its
seal among the run's seals, an unwrapped one by the order of its unwrap among the run's
unwraps. A derived or host-generated P-256 key is designated by the position of its
`SECRET_KDF` or `MEDIA_P256_KEY` delivery. The tape binds each designation together
with its policy. The canonical arguments of
`SEND_SECRET`, `SECRET_MAC` and `SECRET_KDF` hold the designation, never the bytes or any function of them, because an
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
or the CPU's vector level, so admission accepts a recording on either engine and on any CPU
the build supports. It exits 2 with `replay: refused: REASON`.

**Another build (2026-11-23, Spec Q1).** Steps and slice ends are a build's own
([SEM-8](spec/semantics.md#sem-8-determinism), [CAP-9](spec/capabilities.md#cap-9-recording-is-outside-the-program)):
the same program may take a different number of steps on another engine or optimisation
level, so its slices may end at other points and a step bound may stop it elsewhere. A
recording build is the same build as the build without recording, and replay on that build
is exact. A replay on another build of the same revision is admitted and is exact for a run
that took no slice end and was stopped by no step bound. For any other run it may end
`Diverged` where the two builds' steps first differ: a scheduler choice made at a slice end,
or an `OutOfTime` at another point, is then not the one recorded. A recording also holds
its **build digest**: the hash of the compiler, the toolchain and runtime archive, the
program's objects and their sources, the target, the engine, the optimisation level and the
link mode. It leaves out the launch policy (grants, directories and the replay plan), so the
launcher-only replay executable of a build, which differs from the recording executable only
in that policy, has the recording's build digest. When the replaying build's digest differs
from the recorded one, the replay writes `replay: build differs` first, so such a divergence
is attributed to the build and not read as a bug in the program.

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
after the header. A divergence appends the scheduler event, recorded frame kind and
available expected and observed fields. Read capacities and sleep deadlines are shown as
numbers. Other argument bytes and outcome payloads are redacted. With `--diagnose`, the
line also includes the source position when one is available. Replay then exits 1. A
`Sealed` replay exits 0.

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
| received plaintext (response bodies, cookies, minted tokens; a child's stdout and stderr; received datagrams) | always, exactly as received | Replay delivers these bytes to the program, so it needs them exactly. |
| a credential fetch ([TLS-7](spec/tls.md#tls-7-credential-grants)) | the access key id and session token, as received plaintext; never the secret key | The key is sealed by the provider. Replay makes a designation with no bytes, as for a grant. |
| a wrapped secret | only as `wrap`'s blob, sealed under the KEK | Replay delivers the blob, and opening it needs the KEK's bytes, which a recording never holds. |
| a SCRAM exchange | the salt, iteration count, nonces and the server's `v=`, as received plaintext; never the proof | The server's messages are deliveries. With the client's first message they let a holder of the tape test password guesses offline, which is one reason every tape is credential-bearing. The proof would give the same, so it is not stored. |
| an opaque key schedule's secrets, or a sealer's, opener's, header mask's or key share's key ([TLS-8](spec/tls.md#tls-8-packet-ciphers-and-key-agreement)) | never; the issue holds the origins it was made from | Replay delivers each seal, open, mask and Finished value from the recording. A wholly virtual schedule writes no frame at all: replay recomputes it from the seed and the recorded peer messages. |
| a key drawn from a host `Random` (a key share, a signing key, an ECDSA nonce) | never; the delivery holds the public key | The draw happens inside the provider and does not pass the replay point. |
| a packet's plaintext on an opaque path | sealed plaintext as the canonical-arguments rule stores it; every successfully opened plaintext in full, including through `open_into` | Replay checks seals and delivers opens. Opened plaintext is whatever the peer sent (media, or credentials sent inside it), which is one more reason every tape is credential-bearing. |
| sent plaintext | as the canonical-arguments rule stores it: 16 bytes or fewer literally, otherwise a digest | Replay only checks sends. |

A replay gets the same results without a secret's bytes. The launcher does not read the
secret's source; the replaying run gets a designation with no bytes behind it. No operation
reads a secret's bytes, so the program computes the same values, and the deliveries that
depend on the secret, the server's replies, come from the recording. `SEND_SECRET` on
replay checks each designation and the digests of `wire` and `marks`, and delivers the
recorded outcome.

## Unit recordings

Specified 2026-11-24 (unit-tapes-spec-2339). **Not built.** Today a recorded run records
its first unit alone in format 13. Slice U1 below makes the tape format 14 holding only the
root stream. Helpers are recorded from U2, and replayed by stream from U4. Until U5,
[CONC-15](spec/tasks.md#conc-15-units)'s Unbuilt list still applies. Everything else in
this section is the end state those slices build.

A program that calls `task.spawn_unit` runs as several units, and each unit has its own
order of events with no order across units ([CONC-10](spec/tasks.md#conc-10-event-order)).
A unit's behaviour is a function of its entry and of its inputs in the order it takes them
([CONC-15](spec/tasks.md#conc-15-units)). A recording of such a run is therefore one
**stream** per unit. Each stream records that unit's inputs in that unit's own order, and
replays without the other units' streams. No frame orders one unit's events against
another's, and nothing in a recording promises such an order. The total-order promises of
CONC-10, SEM-8, CAP-8 and CAP-9 are per unit and are not widened here.

### What each stream holds

The **root stream** is the stream a format 13 tape holds today: the header, the grants and
the run's frames, plus the unit frames below. Each **helper stream** holds, in the helper's
own order:

- its **entry** ([CONC-15](spec/tasks.md#conc-15-units)): the code descriptor of `f`
  ([PROG-9](spec/programs.md#prog-9-code-descriptors)), the root limit `n`, the root step
  bound or its absence as the helper received it, and the whole of `x` (its plain bytes and its content, with each
  handle it holds written as a handle snapshot, below). The entry is an input of the helper,
  so it is stored in full, never as a digest;
- every frame a root stream would hold for the same operations: each provider operation's
  issue and delivery, each scheduler choice, each `WAKE`, `CANCEL`, `RETIRED` and `CLOSE`,
  with the same per-operation contents as the tables above. A helper's `MEDIA_AGREEMENT`
  verdict ([TLS-8](spec/tls.md#tls-8-packet-ciphers-and-key-agreement)) is a frame of the
  helper's stream, exactly as a root's is of the root stream;
- each **take** (below);
- each **unit operation** the helper issues (below);
- its `END`: the helper's result, with its content and handle snapshots, or its causes
  with their sites and payloads, as `join` delivers them.

A run stop other than `Deadlock` writes no `END` to any stream, and each then replays
`Truncated`, as a root's does today. A `Deadlock` `END` is written to the stream of the
unit that stops.

**Run-wide marks.** An `import_srtp` or `import_traffic` passes the replay point in every
unit, whatever its secret's bytes on replay: its issue holds the designation and profile,
and its delivery the schedule origin or the refusal. A `Spent` found at the run's atomic
mark is that delivery, not a refusal before issue. Replay supplies it from the stream and
never reads the mark ([TLS-8](spec/tls.md#tls-8-packet-ciphers-and-key-agreement) gives
the order).

**Agreement registry entries.** The retirement of a registry entry
([TLS-8](spec/tls.md#tls-8-packet-ciphers-and-key-agreement)) depends on the run's share
count and on drops in every unit, so it is an input of the unit whose limit paid the
entry's 48 bytes, wherever the last counted share ends. Its refund is a `refund` take
(below) in that unit's stream, at the take point where the unit applies it, or at the drop
itself when the retiring drop is that unit's own. Replay applies the refund at that take
and never retires an entry from the replaying run's share count. The replaying units share
one registry object, so a count read there follows replay timing and is not an input.

**Virtual capabilities across units.** A capability of virtual construction
(`provide.clock`, `random`, `random_tape`, `process`, `dir`, `net`, `stdout`, `stderr`, and
every capability a served port issues or a narrowing of one makes) is plain and `cross`
([CONC-2](spec/tasks.md#conc-2-the-send-bound)), so it can reach another unit, and its
cell or port is then reached from more than one unit. Its **home** is the unit that ran its
constructor. Core's rule that a virtual provider's results are recomputed
([core](spec/core.md#provide)) holds only while one unit uses the cell:

- every operation on it issued by a unit other than its home passes the replay point in
  the caller's stream, an issue and a delivery, with the reply supplied on replay. A
  served port's call is answered by its server, a cell's operation from the cell; either
  way the result reaches the caller as a host provider's would;
- in the home unit, an operation on the cell after the first operation another unit
  issued on it also passes the replay point, an issue and a delivery in the home stream.
  For a served port that is each `provide.next` and its `Op`; each reply is checked as an
  issue. Operations before that one are recomputed. On replay the home unit recomputes
  operations on the cell until its stream holds an issue for one, and from that issue on
  supplies every operation on that cell from the stream; an operation with no frame after
  that point ends `Diverged(Missing)`;
- for [TLS-8](spec/tls.md#tls-8-packet-ciphers-and-key-agreement), a virtual `Random`
  whose operation passes the replay point by these rules counts as a host `Random`: a
  `key_share` or `p256_key` from it passes the replay point as a whole and its key has no
  bytes on replay. No drawn key byte is written.

The runtime already lets these capabilities cross: the cell sits behind the run's shared
registry (`Registry::for_unit` shares `run`), and only virtual `Net` sockets are stopped at
the crossing (`UnitSocket`).

**Takes.** A unit **takes** an arrival when it applies it: when the arrival leaves the
unit's queue of arrivals that have reached it and changes the unit's state. That happens in
`take`, in the single-item receive fast path `take_single_item`, and in `release_take`,
which takes in port order up to a retirement notice and leaves later arrivals queued.
Draining the port into that queue is not a take. Every arrival taken is one `TAKE` frame,
in the order the unit took them.

A unit takes only at a **take point**: the scheduler's pump, between tasks or at an idle
wake, and, inside an operation, each pump of `conn.send`, `conn.try_send`, `conn.receive`,
`conn.try_receive`, an inbox's cleaning and its seal wait, a helper's `join` or cleaning
wait, and the drain of the unit's refund mailbox at an admission. Several take points can
fall between two events, so a `TAKE` carries the gap of events since the previous frame, as
a `WAKE` does, and the take point's count: the number of in-operation take-point positions
the unit's code reached since its last event or frame, counted whether or not anything had
arrived there. A scheduler pump, between tasks or at an idle wake, adds nothing to the
count; a take there carries the count the unit has reached. A `TAKE` also carries its
**point kind**, 0 for an in-operation take point and 1 for a scheduler pump, because a take
at the count's in-operation position and one at the next pump after it have the same gap
and count. Replay applies every `TAKE` recorded for a position when it reaches that
position, in stream order: a kind 0 take at the in-operation position with that count, and
a kind 1 take at the first scheduler pump after it, which is the idle pump when every task
of the unit waits. A `TAKE` adds no event, poll or provider operation, so [RL1](#laws) still
holds. Its body is one of:

| arrival | what the `TAKE` holds |
|---|---|
| item | the inbox's ordinal; the item's plain bytes and content in full; a handle snapshot for each handle it holds |
| close | the inbox's ordinal (its last client was cleaned in another unit) |
| seal answer | the ordinal of the connection whose inbox this unit cleaned. When it comes depends on how many sends other units had accepted before the seal, so the replaying unit takes the recorded answer and makes none of its own |
| settlement of a waiting send | the send's issue position in this stream; `Accepted` with each credit's limit ordinal and amount, `Closed` with the returned item in full, or `Withdrawn` |
| helper end | the helper handle's ordinal; the result in full with its handle snapshots, or the causes; and each survivor charge the owner keeps ([CONC-15](spec/tasks.md#conc-15-units)): every open connection, `Shared` node, held `Secret` designation, live key object or unretired agreement registry entry ([TLS-8](spec/tls.md#tls-8-packet-ciphers-and-key-agreement)), and unretired `UdpSocket` pool or group queue the helper paid for, each with its ordinal and amount |
| credits | each credit's limit ordinal and amount |
| last release | the connection's ordinal, its retained header and its unused room |
| shared release | the `Shared` node's ordinal in its home unit |
| cancel | nothing: a cancellation request for this unit's root |
| refund | the limit ordinal and amount of a refund another unit posted to this unit's mailbox, or of an agreement registry entry's retirement (above): a `Secret` designation, key object or registry entry whose charge this unit paid and whose holder ended elsewhere. A post that carries a UDP retirement is its `RETIRED` frame, not a refund take |

Each `TAKE` body starts with its arrival tag: item 0, close 1, seal answer 2, settlement 3,
helper end 4, credits 5, last release 6, shared release 7, cancel 8, refund 9. A
settlement's outcome tag is `Accepted` 0, `Closed` 1, `Withdrawn` 2; a helper end's is a
result 0, `Failed` 1, `Cancelled` 2. Any other tag is corrupt.

A **limit ordinal** numbers the limit records a unit makes in its own order, its root limit
first. A connection, inbox, client, helper handle and `Shared` node gets an object ordinal in
the unit that holds it, in the one sequence per stream that every object ordinal uses:
`conn.new` and `shared.new` allocate at their operation, a handle that arrives allocates at
its take, and none is reused.

**Handle snapshots.** A handle that moves into a unit, in an entry, an item, a returned
item or a helper's result, is written where it arrives as its kind, its new ordinal, and
what the receiving unit can observe of it without a host operation:

| handle | snapshot |
|---|---|
| `Client[T]` | the connection's identity as the inbox unit names it (that unit's stream number and its connection ordinal), and its `cap` and room `b`, so a replay recomputes a send's `Bounds` failure |
| `Shared[T]` | the payload in full, and its home unit's stream number and node ordinal |
| `Socket`, `Listener`, `UdpSocket` | its direction, the bound or peer address, the negotiated protocol, peer digest and binding where they exist, and the readiness and protocol state already observed |
| a capability | its kind and its rule list in the grant snapshot encoding above (a narrowed `Net` holds its narrowed rules) |
| a capability of virtual construction | its kind and its home unit's stream number; no rule list and no cell state. Every operation on it in the receiving unit passes the replay point there (virtual capabilities across units, above) |
| `Secret` designation | its designation (below) and its canonical policy |
| `KeyShare`, `Schedule`, `Sealer`, `Opener`, `HeaderMask` | its origin (stream, position) and profile; a schedule's stage, generations, retired spaces and sender ledger; a sealer's floor and highest sealed counter per domain; never key bytes |
| `Args`, `Child`, `Unique`, `Site`, `StaticFn`, a plain `fn` | as plain bytes; a function as its code descriptor |

A snapshot starts with its kind tag: `Client` 0, `Shared` 1, `Socket` 2, `Listener` 3,
`UdpSocket` 4, capability 5, virtual capability 6, `Secret` 7, `KeyShare` 8, `Schedule` 9,
`Sealer` 10, `Opener` 11, `HeaderMask` 12. Any other tag is corrupt. Plain values have no
tag: the receiving type's layout says where they are. Because capabilities are plain, the
`cross` descriptor of a value lists every capability word it holds, inside list elements,
plain variants and `Shared` payloads included, so the recorder can snapshot each one.

A `Secret` or key object that arrives has no bytes on replay in the receiving unit, whatever
it had where it was made, so every use of it there passes the replay point with an issue
and a delivery.

Later host operations on a handle that arrived are issues in the receiving unit's stream,
checked and delivered there like any other. A replayed snapshot designates nothing on the
host, as every replayed handle does. A socket's `RETIRED` frame is in the stream of the
**refunded unit** that [NET-16](spec/net.md#net-16-udp-sockets-and-batches) names, at the
position where that unit folds it, and it names the socket by the stream number and
ordinal of the unit that dropped it.

**Designations across units.** A designation is the stream number and the tape position of
the delivery that made it, so the order of seals, unwraps and derivations is counted in
each unit's own order. A designation that arrives keeps its origin. The canonical arguments
of `SEND_SECRET`, `SECRET_MAC` and `SECRET_KDF` bind the full designation.

**Unit operations.** These are frames of the unit that performs them. They are checked on
replay like a provider issue, and each outcome that depends on another unit's state is
supplied from the stream:

| operation | at issue (checked on replay) | at delivery (supplied on replay) |
|---|---|---|
| `UNIT_SPAWN` | canonical args = the code descriptor of `f`, `n`, and `x` by the canonical-arguments rule | the new helper's stream number, as an `OP` |
| `CONN_SEND` | client ordinal; the item by the canonical-arguments rule | `Accepted` or `Closed` at once as an `OP`; a send that waits is an `ISSUE`, and its settlement is a later `TAKE` |
| `CONN_TRY_SEND` | as `CONN_SEND` | `Accepted`, `Full` or `Closed`, as an `OP` |
| `UNIT_CANCEL` | the helper handle's ordinal | nothing |
| `CLOSE` of a client or inbox | the ordinal and part, as for every dropped host handle | the outcome |

A refused `spawn_unit` is no frame: its admissions are against the spawner's own limits,
and replay recomputes them. The helper's step bound is not checked: it is the spawner's
remaining steps, which another build may count differently ([Another build](#replay)), and
the helper's stream holds the bound it received.

A send is an output, so its item is checked, not stored: 16 bytes or fewer literally,
longer as a digest, as every sent buffer is. Whether a send is accepted at once, and whether
a `try_send` finds room, depends on how far another unit has received, so it is an input
and comes from the stream. A `CLOSE` of the last client or of an inbox is matched by its
ordinal and part, not by its position, as **Child processes and Unix sockets** in
[What a recording holds](#what-a-recording-holds) says for every dropped handle. A refund
a unit sends to another unit is no frame in the sender's stream: the sender's replay
recomputes it from the charges and the drops it checks, and drops it at the other unit's
mailbox as it drops published arrivals. The unit that receives it takes it as a `credits`
or `refund` take.

`conn.receive`, `conn.try_receive`, `conn.new`, `conn.client`, `shared.new` and
`shared.share` are not frames. What they observe is the unit's own state after the takes
the stream supplies, so replay recomputes them: an `Empty` from `try_receive` follows from
the takes before it, and a `Capacity` from `receive` from the unit's own limit.

### Replay of a run with helpers

A replay opens every stream of the tape and starts each as its own unit, with its entry
from its stream: the root from the header, a helper from its stream's entry. It does not
wait for the parent to reach the spawn, and it may start streams in any order or in
batches, since none depends on another. A helper whose entry's code descriptor does not
resolve in the replaying build, which only `--diagnose` across revisions can meet, ends
`Diverged(Op)` at position 0 of its stream. A full replay requires a stream for every stream
number a replayed `UNIT_SPAWN` delivery names. For one the index does not hold, it writes
`replay: Truncated at 0 in UNIT`, naming it from the spawner's name and spawn index, and
exits 1. A stream whose parent stream is absent is named `?.<stream number>` and sorts
last. `--unit` skips this. Each unit replays against its own stream alone:

- every take comes from its `TAKE` frames, at their positions. Arrivals that other replaying
  units publish are dropped at the port and never taken;
- every unit operation is checked at issue and its outcome supplied, as for a provider issue;
- `UNIT_SPAWN` makes a helper handle whose end, result and causes come from the parent's
  own `TAKE` of the helper's end. It starts nothing: the child is already replaying from its
  own stream;
- `UNIT_CANCEL` and the drops of clients and inboxes are checked and reach no other unit.

So no unit's replay depends on how the others are scheduled or on how many workers run
them. A replay at any worker count of a recording made at any other worker count makes the
same observations in every unit. `hirsel replay app --tape PATH --unit NAME`<!-- doc-ref: planned --> replays one
unit's stream alone and leaves the other streams unread.

A **unit name** is fixed by the program and its inputs, never by a pool id: the root is
`root`, and a helper is its parent's name, a dot, and the helper's 0-based index among its
parent's `UNIT_SPAWN`s that started, so `root.2.0` is the first helper of the root's third.
A helper stream records its parent's stream number and spawn index, and replay derives each
name from those.

Replay does not compare one stream with another: it does not check that an item one unit
sent equals the item another took, or that a parent's `UNIT_SPAWN` arguments equal its
child's entry. Each unit's replay is exact for what that unit observed, which is the only
thing a replay promises, and a tape is the recorder's own file
([What a recording holds](#what-a-recording-holds)). The block check covers the tape id,
so a block from another recording is `Damaged` (format, below). That the streams of one
recording agree is a property of the recorder, held by law RL9 below. The missing-stream
check above is not such a comparison: it checks the tape for an outcome the recorder is
specified to produce, a stream for every helper that started.

**Verdicts.** Each stream ends with one verdict from the [verdict table](#replay), plus:

| verdict | meaning |
| --- | --- |
| `Diverged(Take)` | a recorded take cannot apply at its position (the unit holds no inbox, waiting send or helper with that ordinal, or the item's layout does not match the receiving type); or the unit passes a recorded take's position without reaching its take point; or every task of the unit waits and its next frame is neither a `TAKE` at this point nor a delivery, wake or `END` |

A verdict other than `Sealed` ends that unit's replay at its position, as for a run today.
The other units go on to their own ends, since none takes its inputs from it. When every
stream has ended, the replay writes one line for each stream whose verdict is not
`Sealed`, ordered by spawn-index path, compared numerically (`root.2` before `root.10`):
`replay: VERDICT at N in UNIT`. N counts frames in that
stream after its entry. A replay whose streams are all `Sealed` writes nothing and exits
0. Otherwise it exits 1.

**Truncation.** Each stream is truncated alone. A writer queue that cannot take a stream's
block truncates that stream there and no other, and a crash leaves each stream ending at
its last complete block. The launcher writes `record: truncated at N in UNIT: REASON` once
per truncated stream, ordered by spawn-index path, compared numerically. Its replay ends `Truncated` at that position.

**Credentials.** A taken item, a helper's entry and its result are received plaintext, so
they are stored in full and the recording stays credential-bearing. A `Secret` that moves
between units moves as its designation, never as bytes.

### Unit stream format

Format 14 is the clean bump that unit streams need. A reader of format 14 refuses every
earlier format, and format 13 has no reader once U1 lands (G-1 = B: no store format
continuity).

- Every block's head gains a `u32` **stream number** after its magic, and its sequence
  number counts that stream's blocks. The block check covers the stream number, so a block
  moved between streams fails its check. The root stream is number 0. A helper's number is
  allocated when its spawn starts and is unique in the file. It is not part of the unit's
  name and need not be dense or in spawn order.
- A helper stream's first block is a **stream header**: its parent's stream number, its spawn
  index, and its entry.
- `TAKE` is frame kind 14 (kind 10 is `END`). Its head's task bit is 0: a take names no
  task. Its fields, in order: the gap varint, the point kind byte (0 or 1; any other value is
  corrupt), the count varint, then the body with its arrival tag (above). `UNIT_SPAWN`,
  `CONN_SEND`, `CONN_TRY_SEND` and `UNIT_CANCEL` are operation codes 162, 163, 164 and 165
  of `ISSUE`, `DONE` and `OP`, in `hirsel_core_ids::host_op`, whose table refuses a
  duplicate code.
- An entry, a `TAKE` body, an `END` result or a survivor list longer than one chunk
  continues in `DATA` frames, as an `Exec` grant does.
- Blocks of different streams interleave in the file in whatever order the writer took them.
  A replay indexes the file in one sequential pass at open, recording each stream's blocks
  in sequence, and then reads each stream from its own blocks. Discovery still never
  resynchronises. The index requires each stream's sequence numbers to count from 0 without
  a gap. A block failing its check, a sequence gap, a second header for one stream number,
  or any byte after every indexed stream's `END` writes the file line
  `replay: Damaged at block B` and exits 1, whatever the stream verdicts. A trailing
  partial block is `Truncated` for each stream without `END`. The `END` and `TRUNCATED`
  flags are per stream.
- `tape show` prints the header, then each stream under its unit name, ordered by
  spawn-index path, compared numerically.

### Prior art

| system | what it does for many actors | taken? |
|---|---|---|
| rr (`/tmp/ref-rr` at `b3e8e72`) | One trace per recording with substreams (`src/TraceStream.h:76-87`: events, raw data, mmaps, tasks), and every event frame tagged with its thread id and a single global time (`src/TraceFrame.h:33-40`). That one order exists because rr runs one thread at a time while recording (`src/Scheduler.cc:132`), and replay follows its recorded decisions (`src/Scheduler.h:20-60`) | **ADAPTED.** One file holding several streams is taken, as are tagged frames. The single global order is **NOT TAKEN**: it serializes every thread through one core while recording, which would undo the point of units, and Hirsel has no cross-unit order to record (CONC-10). Weakness avoided: recording cost grows with parallelism. Win taken: one artifact per recording. |
| TigerBeetle VOPR (`/tmp/ref-tigerbeetle` at `a2a3c70`) | Records nothing. The whole cluster, replicas and network alike, is a deterministic function of one seed (`src/vopr.zig:127-135`), and the network is a simulator drawing loss, replay and partitions from that seed's PRNG (`src/testing/packet_simulator.zig:193`, `:272-304`) | **NOT TAKEN** for recordings: a live Hirsel run's inputs come from the host, so they cannot be regenerated from a seed. It stays the model for `hirsel test --sim`, which already plays that part. Win noted: replaying a single actor needs only its own inputs, which is what per-unit streams give. |
| Antithesis-style deterministic hypervisor (public documentation only, no source) | Runs the whole system on one deterministic machine and replays it from its seed and injected inputs, so every actor shares one order | **NOT TAKEN**, for the same reason as rr's global order: it needs a whole-machine single order, which units do not have and must not acquire. |
| Goose (`/tmp/ref-goose` at `d5ae5bb`) | Threads share no mutable memory and meet only through typed queues of flat values (`docs/reference.md:3842-3870`); it has no record or replay | **ADOPTED** as confirmation of the input model: when actors meet only through queues, each actor's queue takes are its whole outside input, and that is the `TAKE` frame. |

### Build slices

| slice | size | depends on | builds | laws |
|---|---|---|---|---|
| **U1** format 14 | S | — | `crates/hirsel-tape-format`: stream number in the block head, stream header, `TAKE` (kind 14, point kind, arrival and snapshot tags), unit operation codes 162-165, `DATA` continuation of entries, takes, results and survivor lists; the open-time index with its `Damaged` rules (sequence gap, duplicate header, failed check, trailing bytes); `tape show` by spawn-index path; `FORMAT = 14`, format 13 refused; the recorder writes the root stream only | RL6 |
| **U2** helper streams | M | U1 | each helper gets its own recorder at its spawn; its entry, host frames and `END` go to its stream; the writer carries several recorders into one file; per-stream truncation and its report; no `END` at a run stop other than `Deadlock` | RL7's recording half, RL12 |
| **U3** takes and unit operations | M | U2 | `TAKE` at every take point and all three take paths, with gap, point kind and count, the refund mailbox drain included; refund takes for posts from other units and for registry entry retirements; the unit operation frames; handle snapshots, with capability rows in the `cross` descriptor (`hirsel-core-layout` `cross.rs`, today without one) and key-object and virtual-capability snapshots; limit ordinals; designations by stream and position; imports always passing the replay point, `Spent` as a delivery; a virtual cell's home and its first foreign operation, and the replay point for foreign and later home operations on it; `RETIRED` in the refunded unit; key objects and registry entries in the helper-end survivor list | RL8, RL9 |
| **U4** replay by stream | L | U3 | open every stream; start each unit from its entry; takes and unit outcomes from the stream; drop published arrivals and posted refunds; `UNIT_SPAWN` handles ended by the parent's `TAKE`; the missing-stream check; `Diverged(Take)` with its three cases; registry entries retired only at their refund takes, never by the replaying registry's share count (`ShareTicket::replay`); per-stream verdicts ordered by spawn-index path; `--unit` | RL7, RL10, RL11 |
| **U5** retire the Unbuilt text | S | U4 | remove CONC-15's Unbuilt recording bullet, TLS-8's root-only sentence and T14's "when unit recordings are built", and the "Not built" line above; rewrite the `crates/hirsel-core-rt/src/units.rs` header to point here | the T14 helper case |

### Laws

Each law's cases go in an existing harness: the tape laws (`crates/hirsel-core-rt/src/tape/tests.rs`)
for frames, the unit ledger law trace (`crates/hirsel-core-rt/src/units_ledger_law.rs`) for
take order, and `hirsel test --replay` for whole programs.

| law | bug | oracle and setup |
|---|---|---|
| **RL6** format 14 bytes | a stream number outside the block check, a `TAKE` that decodes to another arrival, a format 13 tape admitted | Hand-built byte strings: a block of stream 3 whose stream number is changed to 4 fails its check; each `TAKE` body in the table decodes to the arrival its hand-written bytes name; a valid format 13 tape is refused at open. Hand-built tapes with a sequence gap, a duplicate stream header, a flipped byte in a middle block and a byte after the last `END` are each `Damaged` at the hand-counted block; a `TAKE` with point kind 2, arrival tag 10 or snapshot tag 13 is corrupt; a 2 MiB entry round-trips through `DATA`. |
| **RL7** each unit replays alone | a helper's input that is not in its stream (an unrecorded take path, an entry stored as a digest) | A program whose root spawns two helpers that exchange items over connections, with one helper also doing a host `RANDOM`. Record it; replay each unit with `--unit` and every other helper stream's blocks deleted from the file. Each replay is `Sealed`, its scripted provider records zero calls, and each unit's printed values equal the hand-derived ones. Further cases: a helper is handed a virtual sealer that already sealed counter 5; alone it replays `seal(5)` as `Reused` and `seal(6)` to the recorded bytes, and RL5's search of the tape finds no key bytes. A helper draws from a `provide.random` made by the root while the root draws too; each unit alone replays its recorded draws, and the scripted provider records zero calls. |
| **RL8** take positions | a take applied at another point of the unit's order than it was recorded | Using the ledger's hold hook, script when each arrival reaches a unit between its events. The recorded `TAKE` gaps equal the hand-counted event positions, and on replay each `try_receive` gives the hand-tabled `Ok` or `Empty` at each step, including a `release_take` that takes an older item before its retirement notice and two `try_receive` calls with no event between them, the second of which takes. A separate case, with the hook off, has one item published while its receiver is ready, so `take_single_item` takes it: its `TAKE` has the hand-counted gap and point, and replay delivers it at the same `receive`. One receiver is recorded twice, the hold hook releasing the item before the first pump and after an idle wake: both tapes hold the same hand-counted (gap, count), with point kinds 0 and 1, and each replays to the hand-tabled results. A `try_receive` that is `Empty`, a pump with no event between that takes the item, and a second `try_receive` that is `Ok` replay to `Empty` then `Ok`. |
| **RL9** streams of one recording agree | a recorder that writes an item one unit sent differently from the item another took, or an entry unlike its spawn | For each RL7 and RL8 recording, a test-only reader pairs every `CONN_SEND` with the `TAKE` it reached, and every `UNIT_SPAWN` with its child's entry. The sent items' canonical arguments equal the canonical arguments of the taken items, in connection order. The oracle is the hand-written list of sends. |
| **RL10** worker count changes no replay | a replay that depends on scheduling across units | Record the RL7 program at `HIRSEL_UNTIED=1` and at `HIRSEL_UNTIED=8`. Replay each recording at both counts. All four replays are `Sealed`, with outputs equal to the hand-derived values. |
| **RL11** a helper's divergence | a divergence that stops other units, goes unreported, or is reported in a timing-dependent order | Replay RL7's recording with the program edited so that (a) helper `root.1` sends other bytes, (b) helper `root.0` receives on an inbox it never made, and (c) both at once. (a) ends `Diverged(Arg) in root.1` and (b) `Diverged(Take) in root.0`, each at the hand-counted position. (c) writes both lines in name order. Every other stream is `Sealed`, and the exit status is 1. (d) RL7's tape with `root.1`'s blocks removed, replayed without `--unit`: `Truncated at 0 in root.1`, exit 1, other streams `Sealed`. (e) `root.0` edited to receive one extra item ends `Diverged(Take) in root.0` at the hand-counted position, exit 1. |
| **RL12** recording with helpers changes nothing | unit recording that adds a charge, step, poll or event | RL1's corpus gains the RL7 program. Its outputs, `Capacity` refusals, step counts per unit and provider call logs are equal across the three builds of RL1. |

The T14 case for helpers is
`helper_virtual_agreement_verdicts_are_recorded_and_replayed` in [TLS](spec/tls.md#laws), built in U5:
two helpers hold different virtual X25519 shares and construct against each other's public
key. Each verdict is in its own helper's stream, and each helper replays alone without
consulting the agreement engine. Two more cases go with it in the tape laws. Two helpers hold aliases of
one sealed `srtp_import(receive)` secret and both import: each `--unit` replay gives its
recorded result, exactly one of them `Spent`, whichever stream starts first. The root
constructs a `tls13` schedule while a helper still holds a counted share, and the helper
drops it: the root's stream holds a `refund` take of 48 bytes at the hand-counted position,
and the root replayed alone makes a 48-byte admission that is refused before that take and
admitted after it, as live.

## Format

A recording file is a **tape** in format 13, which `crates/hirsel-tape-format` owns. Format 7
added the run's Map hash key to the header, and format 12 removes it (below). Format 8 adds the terminal byte to stream grants
and adds the SCRAM frames described in [TLS-6](spec/tls.md#tls-6-scram-sha-256). Format 9
writes each Secret grant's policy as its canonical set of uses
([TLS-5](spec/tls.md#tls-5-secrets)). Format 10 gives `SEND_SECRET` the list of
designations, the `wire` and the `marks` of one message. Format 11 adds the `Cred` grant
record and the credential fetch frames ([TLS-7](spec/tls.md#tls-7-credential-grants)).
Format 12 removes the Map hash key from the header. Format 13 ends a Run's launch metadata
with the recording's 32-byte build digest. A replay refuses a tape of an earlier format. The format is not part of the language and a replay reads only tapes its own build admits,
and its pinned executable reads that layout. The host uses the checked block-zero prefix
through the launch arguments to find and verify the pin.

- The header holds no Map hash key. Every run, a replay included, draws its own key from
  host entropy at entry, and no observable depends on it
  ([SEM-5](spec/semantics.md#sem-5-maps), [CAP-8](spec/capabilities.md#cap-8-the-replay-point)),
  so a replay supplies nothing for it and compares nothing with it.
- The header has no mode byte. Its grant snapshot stores the run's root Net rules in order
  and with duplicates; each address rule occupies 23 bytes.
- The `Tls` rule record holds the direction byte 2; the trust byte, followed by 32 bytes of
  roots digest when the trust is `Roots`; the port interval; and a `u16` count of patterns,
  each a length byte followed by its bytes.
- A `Secret` grant record holds the parameter name, the source label and the canonical
  policy text ([TLS-5](spec/tls.md#tls-5-secrets)), all length-prefixed.
- A `Cred` grant record, after the `Secret` grant records, holds the parameter name, the
  source text and the canonical policy text
  ([TLS-7](spec/tls.md#tls-7-credential-grants)), all length-prefixed. A fetch's issue holds
  the spare capacities of `key_id` and `token` and its deadline. A successful delivery holds its own tape position as `value` (the designation
  the key gets) and, as the payload, the key id and the session token, each
  length-prefixed, then the expiry as a little-endian `u64`. The key is never in a frame.
- Buffered issue arguments start with tag 18 and two varint spare capacities. The
  second is zero for single-buffer operations. The remaining operands follow in the
  usual literal or digest encoding.
- A `CLOSE` is an immediate OP with task zero. Its frame keeps its position for
  diagnostics. Replay matches it by ordinal and part.
- The `GRANT` frame of a `Stdin`, `Stdout` or `Stderr` grant holds its terminal byte.
- The `GRANT` frame of an `Exec` grant holds the total profile snapshot length, then its
  first payload chunk. `DATA` frames carry the remaining bytes.
- An `Env` grant record holds the pattern list and the sorted name and value pairs. Values
  retain their OS bytes, including when they are not UTF-8.
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
| **RL1** recording changes nothing | recording machinery that adds a charge, a step, a poll or a second read, or leaks into a build without recording | For a corpus of small programs with scripted providers and ordered senders, run the build without recording, the recording build with recording off and the same build with recording on, on both engines. Their outputs, outcomes, cause sites and chains, `Capacity` refusals, step counts and provider call logs are equal. The executable built without recording defines no symbol of the recording runtime. |
| **RL2** a divergence ends the replay at its position | a replay that continues past a mismatch, or reports it to the program | Record a run, then replay it with the program edited to (a) keep to the end a handle the recording dropped (its recorded `CLOSE` never comes: `Diverged` at the run's end), (b) omit its last send and (c) send other bytes, each still returning the recorded result, and (d) read into a buffer too small for a recorded delivery. Each replay ends `Diverged` at the hand-counted frame position, exits 1, and its scripted provider records zero calls. Replays with the program edited to (e) drop a handle at another point and (f) read into a larger buffer, each still delivering the recorded bytes, end `Sealed` (2026-11-23, Spec Q5). |
| **RL3** replay is offline | replay reaching the host or needing keys | Record an HTTPS GET against the loopback server. Replay it with a refuse-all inner provider, the server stopped, and the roots file removed. The response bytes, the protocol and the peer digest are equal to the recorded run's, and the inner provider records zero calls. |
| **RL4** replay without the secret | replay reading the secret's source | Replay TL10's recording ([TLS](spec/tls.md#laws)) with the environment variable unset. The run starts. Its outputs equal the recorded run's, byte for byte. The inner provider records zero calls. |
| **RL5** a secret never reaches a recording | a secret's bytes in a frame or a digest | Record TL10's run and search the recording for the value, its base64 and its raw bytes: there are no hits. |
