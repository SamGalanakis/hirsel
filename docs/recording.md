# Recording and replay

A recording is a quick way to keep a trace of one execution for debugging and testing,
without altering the code. The launcher and the test runner make it, and replay is a tool
run against it. No program operation takes, returns or mentions a recording
([recording is outside the program](spec/capabilities.md#cap-9-recording-is-outside-the-program)), so this page is the
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

A program whose entry reaches a foreign function has no recording build
([foreign code cannot be recorded](spec/foreign.md#ffi-8-a-program-that-reaches-foreign-code-is-never-recorded-replayed-or-reused)).
Recording and replay commands refuse it before building, reading a tape or running native
code: `hirsel replay` exits 2 with a refusal message, `hirsel test --replay` fails each
foreign-reaching test alone, and `hirsel test --laws` runs those tests in the ordinary
build and reports them as unobserved.

`--release`, `--backend` and `--target` select the engine, optimisation and target of a
recording build as they do for any other build.

A recording executable started with the environment variable `HIRSEL_RECORD=PATH` records
into PATH. The runtime reads the variable and removes it from the environment before the run
starts, so no grant observes it. Started without it, the executable runs with recording off.
An executable built without recording never reads it.

**Same observations.** Given the same values at the
[replay point](spec/capabilities.md#cap-8-the-replay-point) in the same order, a build without
recording, a recording build with recording off and the same build with recording on make the same
observations ([determinism](spec/semantics.md#sem-8-determinism)). Recording runs outside the
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
- each scheduler choice ([event order](spec/tasks.md#conc-10-event-order));
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
buffer is too small for the recorded delivery ends `Diverged` at that issue. So a
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
`Diverged`, the second at the run's end. It then changes local ownership only: nothing is killed,
closed or unlinked on the host. A delivery that its issue could not have produced, such as a
packet whose length and flag contradict its room, is `Corrupt`
([child process replay](spec/exec.md#exec-8-exec-at-the-replay-point-and-served-exec)).

A recording is a file of the recording user. Every child of a run granted `Exec` runs as that
user and can rewrite it, or put another valid tape of the same program in its place. Replay
compares the program against the tape it is given. A `Sealed` verdict does not say which
execution produced that tape.

**Terminal bits.** The `GRANT` frame of a `Stdin`, `Stdout` or `Stderr` grant carries one
byte after its kind: 1 when the stream was a terminal at launch, 0 otherwise
([stdin](../core/std/api/stdin.md#terminals)). `is_terminal` reads it and adds no frames, and
a replay answers from it without probing its own descriptors. A terminal's size is not in
the grant: each `stdout.size` or `stderr.size` call on a stream whose bit is 1 is one
`TERM_SIZE` operation frame holding the answer, and a call on a stream whose bit is 0 adds
none ([stdin](../core/std/api/stdin.md#size)).

**Standard input and environment.** Each `stdin.read` issue holds the spare capacity of
its buffer and its deadline. Its delivery holds the bytes in their original fragment
sizes, or `Aborted`, `TimedOut` or the host error. An Env grant is recorded in the grant
snapshot with its patterns and the launcher's snapshot of the covered names and values.
`env.get`, `env.len` and `env.name` read that snapshot and add no frames. A replay
supplies both from the recording and reads neither the host's standard input nor its
environment. A `stdin.read_secret` is one `STDIN_SECRET` operation: its issue holds
`until` and the policy text, and its delivery holds `NotTerminal`, `Busy`, `Empty`,
`TooLong`, `Eof`, `Aborted`, `TimedOut` or the host error, or on success the new
designation's delivery position. It holds no byte of the line and no length
([credentials](#credentials)). A replay neither reads the terminal nor changes its modes.

**TLS sessions.** A TLS stream is recorded through the `Net` grant it runs over.

| operation | at issue (checked on replay) | at delivery (supplied on replay) |
|---|---|---|
| `TLS_CONNECT` | socket ordinal; canonical args = `name`, trust (`System`, or the SHA-256 of the roots bundle), the ALPN entries in order, `until` | the outcome. On success the new stream ordinal, the negotiated protocol, the 32-byte peer digest and the binding value with its length (0 when absent). |
| `TLS_HELLO` ([server handshake](spec/tls.md#tls-9-server-handshake)) | socket ordinal; canonical args = `until` | the outcome. On success the new `Hello` ordinal and the normalised server name. |
| `TLS_ACCEPT` ([server handshake](spec/tls.md#tls-9-server-handshake)) | `Hello` ordinal; canonical args = the chain's entries in order, the key's designation and policy, the ALPN entries in order, `until` | the outcome. On success the new stream ordinal and the chosen ALPN index. Never the key's bytes. |
| `RECV_BOUNDED` on a TLS half | as for a socket | the decrypted plaintext, in its original fragments |
| `SEND_PAIR` on a TLS half | as for a socket: both offered plaintext buffers and the deadline | the committed count |
| `SEND_SECRET` | half ordinal; canonical args = `until`, each secret's designation in `secrets` order, `wire`, `marks` | the outcome, and the committed count on failure |
| `SECRET_MAC` | run-owned sink at object 0; modes 0/1 for SHA-256, 7/8 for SHA-384 and 9/10 for SHA-512 (sign/verify). Modes 11/12, 13/14 and 15/16 enforce the same algorithms' JWS key minimums. Modes 17/18 sign/verify SHA-1. Canonical args = mode, secret designation and policy, `data` and the input tag for verify. | the 20-, 32-, 48- or 64-byte tag, the verification verdict or the JWS `ShortKey` refusal |
| `SECRET_KDF` ([secrets](spec/tls.md#tls-5-secrets)) | run-owned sink at object 0; canonical args = parent designation and policy, child policy, then `key_prefix` and `data` for HMAC, or the mode, `salt`, `len` and `info` (HKDF) or `cost` (Argon2id), or `alg`, `inner`, `prefix` and `suffix` (digest) | the delivery position or Argon2id's delivered `Invalid`, without key bytes |
| `SECRET_MAC` key operations ([secrets](spec/tls.md#tls-5-secrets)) | object 0; mode 2 `wrap`, 3 `unwrap`, 4 `sign`; canonical args = mode, the key's designation and policy, then for `wrap` the wrapped secret's designation and policy, for `unwrap` the `uses` text and the blob, for `sign` the algorithm, `Random` identity and `msg`, and the spare of `out` | the outcome; for `wrap` and `sign` on success the blob or signature. `Capacity`'s available bytes are the recorded spare. |
| `SECRET_MAC` AEAD ([secrets](spec/tls.md#tls-5-secrets)) | object 0; mode 5 `aead_seal`, 6 `aead_open`; canonical args = mode, key designation and policy, algorithm, nonce, associated data, plaintext or sealed bytes, and output spare | the refusal or the appended bytes, including the authenticated plaintext for open |
| `MEDIA_KEY_SHARE` ([packet ciphers and key agreement](spec/tls.md#tls-8-packet-ciphers-and-key-agreement)) | object 0. Canonical args bind the Random designation, group and public buffer spare. | The delivery position and public key bytes. Replay holds a share ticket with no private key. |
| `MEDIA_P256_KEY` ([packet ciphers and key agreement](spec/tls.md#tls-8-packet-ciphers-and-key-agreement)) | root-owned key generator at object 0. Canonical args bind the Random designation, signing policy and public buffer spare. | The delivery position and 65-byte public point. The Secret’s document stays with the provider. |
| `MEDIA_CAPTURE` ([packet ciphers and key agreement](spec/tls.md#tls-8-packet-ciphers-and-key-agreement)) | The unit's media outcome at object 0. Canonical args bind the operation, public arguments and writable spare. | A child origin, public output bytes, or a refusal. Replay validates the delivery shape before it writes any bytes. |
| `MEDIA_AGREEMENT` ([packet ciphers and key agreement](spec/tls.md#tls-8-packet-ciphers-and-key-agreement)) | registry verdict at object 0 in the constructor's unit. Canonical args bind the constructor's public operands and share origin, including for virtual shares. | `Invalid` (0), accepted (1), or `Issued` (2). Replay supplies the verdict without an agreement provider call. |
| `SCRAM` | half ordinal; canonical args = secret designation, `client_first`, `server_first`, `before`, `after`, `until` | the outcome, and the committed count on failure. The new `Scram`'s ordinal on success. |
| `SCRAM_VERIFY` | `Scram` ordinal; canonical args = `server_final` | the verdict |

A recording **never** holds ciphertext, handshake records, keys, client or server randoms,
the certificate chain, the roots bundle, or the provider's verification time. Nor does it
hold any SCRAM value computed from a password: the normalized password, `SaltedPassword`,
`ClientKey`, `StoredKey`, `ServerKey`, the client proof or the expected server signature
([SCRAM-SHA-256](spec/tls.md#tls-6-scram-sha-256)).

**Secret designations.** A granted secret is a grant. It takes a `GRANT` frame and an object
ordinal in the order of the run's grants, like every other grant. The header's grant
snapshot keeps the parameter name, the canonical policy text, and the source label
(`env:OPENAI_API_KEY`), but not the bytes. A sealed secret is designated by the order of its
seal among its unit's seals, an unwrapped one by the order of its unwrap among its unit's
unwraps. A derived or host-generated P-256 key is designated by the position of its
`SECRET_KDF` or `MEDIA_P256_KEY` delivery. Each designation also names the stream of the
unit that made it ([designations across units](#unit-recordings)). The tape binds each designation together
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
effect. It installs no signal handler, reads no `Secret` source or terminal line and opens no network,
roots or key. Replayed sockets, TLS halves and secrets are local designations: splitting or
dropping one changes only local ownership, sends no `close_notify` and reaches no host.
A replayed `Net` grant starts with no live rules, and its narrowed rule lists and their
`Capacity` charges are computed from the snapshot and each parent's intersections. Launch
metadata is the recorded value, never the digest of the replaying executable.

**Admission.** Before any program code runs, a replay refuses a recording whose entry
identity, [entry code revision](spec/programs.md#prog-9-code-descriptors),
argument snapshot, root limit or charge policy differs from the replaying build's, and one
whose header is damaged. The code revision covers the reachable checked code, not the engine
or the CPU's vector level, so admission accepts a recording on either engine and on any CPU
the build supports. It exits 2 with `replay: refused: REASON`.

**Another build.** Steps and slice ends are a build's own
([determinism](spec/semantics.md#sem-8-determinism),
[recording is outside the program](spec/capabilities.md#cap-9-recording-is-outside-the-program)):
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
`replay: VERDICT at N in UNIT`, where N is the position of the frame it was checked against,
counted after the header, and UNIT is the stream's [unit name](#replay-of-a-run-with-helpers):
`root` for the root stream, whether or not the run had helpers, since the root is stream 0 and
one replay reads every stream. `Damaged at block B` names the file's block and no unit. A
divergence appends the scheduler event, recorded frame kind and
available expected and observed fields. Read capacities and sleep deadlines are shown as
numbers. Other argument bytes and outcome payloads are redacted. With `--diagnose`, the
line also includes the source position when one is available. Replay then exits 1. A
`Sealed` replay exits 0.

### Export replayed output

`hirsel replay PACKAGE --tape TAPE --outputs DIR` writes the bytes the
program recomputed and committed. The operator can also request this on
`replay --state STATE`, including with `--unit`, if the pinned image supports
output export. DIR must be new. Hirsel publishes it with owner-only access
after all selected streams seal. A divergence, unsupported image, or export
failure leaves the destination absent. A pre-existing destination is preserved.

Root files are named `stdout`, `stderr`, and `socket-N`, where N is a
plaintext TCP or Unix stream socket's local object ordinal. Helper files have
a `stream-ID-` prefix. Each file starts at the first committed byte. A partial
send contributes its committed prefix once. TLS, Secret sends, pipes and
datagrams are outside this export. An export may contain application credentials.
The export limit is 64 MiB across 256 files.

## Credentials

A recording holds what servers sent to the run, exactly as received, so a recording of a
run that used a `Net` grant for a TLS session, sent through a `Secret`, or received minted
tokens, `Authorization` headers, `Random` bytes or environment values holds credentials. Every recording is
treated as **credential-bearing**:

- its file is created readable only by its owner, at a path that does not exist yet;
- `hirsel tape show PATH --include-credentials` writes its header and stored frames to
  standard output. Add `--json` to read a JSON array in spawn-path order, with physical
  frames in order within each stream. Its schema-1 `HEADER` is followed by
  `STREAM` records and their entries and frames. Frame positions start at one in each stream. A
  `REPEAT` occupies the positions of its expanded choices. Binary fields
  ending in `_hex` are hexadecimal strings. Use a JSON reader that preserves
  64-bit integers for exact clock values and counters. A `TRUNCATED` record marks an
  incomplete stream. Check the command's exit status before consuming the
  document. Every export of a recording requires `--include-credentials`;
- it is never redacted in place, since a replay needs its bytes exactly.

Where cleartext can be in a recording:

| bytes | in the recording? | why |
|---|---|---|
| bytes supplied by a `Secret` grant | never via the grant, send or `TLS_ACCEPT` record | Replay uses the designation. |
| a sealed secret | only where the bytes were already there, as the received plaintext they were computed from | Replay recomputes the same bytes from those deliveries, so the seal adds nothing. |
| received plaintext (response bodies, cookies, minted tokens; a child's stdout and stderr; received datagrams) | always, exactly as received | Replay delivers these bytes to the program, so it needs them exactly. |
| a credential fetch ([credential grants](spec/tls.md#tls-7-credential-grants)) | the access key id and session token, as received plaintext; never the secret key | The key is sealed by the provider. Replay makes a designation with no bytes, as for a grant. |
| a line read by `stdin.read_secret` ([stdin](../core/std/api/stdin.md#secret-input)) | never, nor its length | The line goes from the terminal into run-owned storage. The `STDIN_SECRET` issue holds `until` and the policy text, and its delivery holds the outcome and, on success, the designation's delivery position. The charge is fixed, so nothing recorded depends on the line. Replay touches no terminal and makes a designation with no bytes, as for a grant. |
| a wrapped secret | only as `wrap`'s blob, sealed under the KEK | Replay delivers the blob, and opening it needs the KEK's bytes, which a recording never holds. |
| a SCRAM exchange | the salt, iteration count, nonces and the server's `v=`, as received plaintext; never the proof | The server's messages are deliveries. With the client's first message they let a holder of the tape test password guesses offline, which is one reason every tape is credential-bearing. The proof would give the same, so it is not stored. |
| an opaque key schedule's secrets, or a sealer's, opener's, header mask's or key share's key ([packet ciphers and key agreement](spec/tls.md#tls-8-packet-ciphers-and-key-agreement)) | never; the issue holds the origins it was made from | Replay delivers each seal, open, mask and Finished value from the recording. Replay recomputes a virtual schedule in the unit that made it from the seed and the recorded peer messages. An arriving schedule takes its operations through the receiving unit's replay point. |
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

Per-helper streams, entries, host frames, `END` frames, takes and unit operations are
recorded, and each helper stream replays as its own unit from its entry. Some takes and
handles do not replay yet: [What this build replays](#replay-of-a-run-with-helpers) lists
them, and [units](spec/tasks.md#conc-15-units) lists what is unbuilt. The rest of this
section describes the full unit-recording design.

A program that calls `task.spawn_unit` runs as several units, and each unit has its own
order of events with no order across units ([event order](spec/tasks.md#conc-10-event-order)).
A unit's behaviour is a function of its entry and of its inputs in the order it takes them
([units](spec/tasks.md#conc-15-units)). A recording of such a run is therefore one
**stream** per unit. Each stream records that unit's inputs in that unit's own order, and
replays without the other units' streams. No frame orders one unit's events against
another's, and nothing in a recording promises such an order. Total-order determinism
guarantees apply per unit and are not widened across units.

### What each stream holds

The **root stream** holds the header, the grants, the run's frames, and the unit frames
below. Each **helper stream** holds, in the helper's own order:

- its **entry** ([units](spec/tasks.md#conc-15-units)): the
  [code descriptor](spec/programs.md#prog-9-code-descriptors) of `f`,
  the root limit `n`, the root step
  bound or its absence as the helper received it, and the whole of `x` (its plain bytes and its content, with each
  handle it holds written as a handle snapshot, below). The entry is an input of the helper,
  so it is stored in full, never as a digest;
- every frame a root stream would hold for the same operations: each provider operation's
  issue and delivery, each scheduler choice, each `WAKE`, `CANCEL`, `RETIRED` and `CLOSE`,
  with the same per-operation contents as the tables above. A helper's `MEDIA_AGREEMENT`
  verdict ([packet ciphers and key agreement](spec/tls.md#tls-8-packet-ciphers-and-key-agreement)) is a frame of the
  helper's stream, exactly as a root's is of the root stream;
- each **take** (below);
- each **unit operation** the helper issues (below);
- its `END`: the helper's result, with its content and handle snapshots, or its causes
  with their sites and payloads, as `join` delivers them.

A run stop other than `Deadlock` writes no `END` to any stream, and each then replays
`Truncated`, as a root's does today. A `Deadlock` `END` is written to the stream of the
unit that stops.

**Run-wide marks.** An `import_srtp`, `import_traffic` or `import_sframe` passes the replay
point in every unit, whatever its secret's bytes on replay: its issue holds the designation
and profile or suite, and its delivery the schedule origin or the refusal. A `Spent` found
at the run's atomic mark is that delivery, not a refusal before issue. Replay supplies it from the stream and
never reads the mark ([packet ciphers and key agreement](spec/tls.md#tls-8-packet-ciphers-and-key-agreement) gives
the order).

**Charges released by a run-wide count.** Five charges are released when the last of
holders that can sit in several units is dropped: an agreement registry entry's 48 bytes
([packet ciphers and key agreement](spec/tls.md#tls-8-packet-ciphers-and-key-agreement)), the charge that `narrow` or
`provide.secret` admitted for a `Secret` designation, and the charge of the sealed bytes
behind one, which `seal` or the derivation admitted and which is released with the last
designation of those bytes, a `Shared` node's charges and a connection's charges.
`share` adds holders, a `Secret` is `cross`, and the counts are
run-wide, so whether a given drop is the last one follows the order of drops in every unit,
which replay does not keep. One rule covers all five, every time, whether or not a holder
ever left the unit:

- the release is an input of the **paying unit**: the unit whose limit admitted the
  charge, or, once that unit has ended with the charge among its survivors, the unit of
  the owner that kept it ([units](spec/tasks.md#conc-15-units));
- for the registry entry, designation and sealed bytes, its refund is a `refund` take
  (below) in the paying unit's stream, wherever the last
  holder ends. When the last holder is dropped in the paying unit itself, the take is at
  that drop, a take point of kind 0. When it is dropped in another unit, the refund is
  posted to the paying unit's mailbox and taken at a drain of it;
- when one drop releases both a designation and the sealed bytes behind it and both
  charges have one paying unit, its stream holds two `refund` takes at that position, the
  designation's first;
- for a `Shared` node and a connection the take is `shared release` or `last release`, not
  `refund`: when the home's or admitting unit's own drop or cleaning is the last, that take
  is in its stream at that drop, a take point of kind 0, and replay never releases either
  from its holder count.
- replay applies each release at its take and never releases any of the five from a count
  the replaying units read, since they share the count objects and a count read there
  follows replay timing.

So a unit that seals, shares to a helper and drops records one `refund` take for its
designation and one for the bytes, at its own drop or at the drain after the helper's
drop, according to which was last live; and its replay alone applies them there.

**Virtual capabilities across units.** A capability of virtual construction
(`provide.clock`, `random`, `random_tape`, `process`, `dir`, `net`, `stdout`, `stderr`, and
every capability a served port issues or a narrowing of one makes) is plain and `cross`
([the `send` bound](spec/tasks.md#conc-2-the-send-bound)), so it can reach another unit, and its
cell or port is then reached from more than one unit. Its **home** is the unit that ran its
constructor. Core's rule that a virtual provider's results are recomputed
([core](spec/core.md#provide)) holds only while one unit uses the cell:

- every operation on it issued by a unit other than its home passes the replay point in
  the caller's stream, an issue and a delivery, with the reply supplied on replay. A
  served port's call is answered by its server, a cell's operation from the cell; either
  way the result reaches the caller as a host provider's would;
- in the home unit, the mark is the **crossing frame**: the first unit operation of the
  home that carries out of the unit any capability whose operations reach the cell or port,
  the original, a narrowing of it, or a capability a served port issued, by the `cross`
  descriptor of the value it sends. That is a `UNIT_SPAWN` whose `x` holds one, a
  `CONN_SEND` or `CONN_TRY_SEND` whose item holds one, or a helper home's `END` whose result
  holds one. It is the crossing frame whatever its outcome (`Closed`, `Full` or a send that
  waits included). Every operation of the home on the cell after the crossing frame passes
  the replay point, an issue and a delivery in the home stream; operations before it are
  recomputed, since the cell's mutex orders them and they see only the home's state. The
  crossing frame is in the home's own order, so replay meets it at the same point it was
  recorded, from the frame alone, with no count read across units;
- a wait on the cell that is pending at the crossing frame (a `provide.next`, a
  `clock.idle`, a sleep, or a served call's `until` on that clock (`provide.rs:722`), which
  is issued under the code that clock's sleep uses) is issued at that frame: right after the crossing
  frame's own frame, each pending wait is an `ISSUE` with its own operation code
  (`PORT_NEXT`, `CLOCK_IDLE`, `SLEEP_UNTIL`, `SLEEP_UNTIL_WALL`), in the order the waits
  registered, with the task bit of the task that waits. Its completion is a `DONE`
  delivery, supplied on replay. For a served port each `provide.next` and its `Op` is such
  an operation and each reply is checked as an issue. On replay, from the crossing frame on,
  every operation on that cell comes from the stream, and one with no frame ends
  `Diverged(Missing)`. A capability that arrives back at its home is used under this rule,
  not the snapshot row's;
- for [packet ciphers and key agreement](spec/tls.md#tls-8-packet-ciphers-and-key-agreement), a virtual `Random`
  whose operation passes the replay point by these rules counts as a host `Random`: a
  `key_share` or `p256_key` from it passes the replay point as a whole and its key has no
  bytes on replay. No drawn key byte is written.

Two cases show why the mark is the crossing frame and not the first foreign operation. The
root makes a `provide.random`, sends it to a helper, and draws twice; the helper draws once
between the root's draws, and the root writes no other frame between them. The root's
`CONN_SEND` comes before both draws in its order, so both are issues with deliveries in its
stream, and its replay alone gives each draw its own recorded bytes. The root serves a port
whose server task is parked in `provide.next` when the root sends the port's capability to a
helper. The `CONN_SEND` is followed by the `PORT_NEXT` `ISSUE` of the parked task, and the
helper's later call reaches it as that issue's `DONE`, so the root replayed alone wakes its
server at the recorded position.

The runtime already lets these capabilities cross: the cell sits behind the run's shared
registry (`Registry::for_unit` shares `run`), and only virtual `Net` sockets are stopped at
the crossing (`UnitSocket`).

**Sockets that cross.** A TCP `Socket` or `Listener` that a recording unit's tape made
crosses to another unit, in an item, an entry or a helper's result. The rules, and why:

- **One naming.** Every recording unit, root or helper, names its taped TCP sockets and
  listeners as `ordinal << 2` carriers of its own stream's objects, and the two low bits are
  the half. A helper whose stream is recorded gets the same socket tape the root gets from
  its `Net` grant, so a helper's `CONNECT`, `LISTEN` and `ACCEPT` make carriers as the
  root's do. An earlier build named a helper's sockets by tape handle through their routes.
  That second scheme is gone: one form lets the crossing code name a socket the same way in
  every unit, and a replay of any stream reads the same carriers. A unit whose stream is cut
  at its entry has no socket tape and keeps raw reactor keys, as it does without a recording.
- **No departure frame.** The unit operation that carries the value (`UNIT_SPAWN`,
  `CONN_SEND`, `CONN_TRY_SEND`, or the helper's `END`) is the departure, and its value holds
  the socket's snapshot. When the socket leaves, its object in the sender's stream no longer
  reaches a source. No other frame is needed, because the operation is already at the
  position the departure happens. A socket that comes back without a take (a send refused
  `Full`, `Closed` or `Bounds`, a waiting send withdrawn or settled `Closed`) keeps the
  sender's own ordinal, which again reaches the socket's reactor key in that unit. That is
  why a `Closed` settlement's value names the sender's ordinals. A socket dropped in transit
  closes with no frame, as a TCP socket's drop has none.
- **Arrival.** The receiving unit's take, or the helper's entry, or the owner's helper-end
  take, gives the socket a new object ordinal in the receiving stream, and its later
  operations there name that ordinal; its halves after a split share it, as in the sender.
  A self-send takes a new ordinal too, as a client sent to itself does. A socket
  arriving where nothing named it (from a unit whose stream is cut) cuts the receiving
  stream.
- **Snapshot source.** The tag 2 and tag 3 bodies come from the sending unit's own stream,
  not from the host reactor. The direction and address come from the `CONNECT` that made
  the socket (the address it named), the `ACCEPT` (the peer address it delivered) or the
  `LISTEN` (the bound address). The state's bit 0 is set once a `RECV_BOUNDED` with room
  delivered no bytes, and bit 1 once an operation on it failed with a reset (error tag 5).
  So a replay of the sender recomputes the snapshot from frames it already checks, and the
  reactor's flags, which the tape never reads, need no new path. TLS is always 0, because a
  socket with a TLS session or half, or one a served `Net` owns, never crosses
  (`UnitSocket`).
- **No survivor kind.** A TCP socket holds no charge: its kernel queues and the reactor
  record are host memory behind provider operations, which charges do not cover
  ([budgets](spec/budgets.md)). Only a `UdpSocket`'s pool is charged, and it already
  survives as kind 5. So a helper's result that holds a TCP socket adds no survivor, and the
  helper-end take keeps the survivor list as it is.
- **Format tag at the TCP crossing change.** The format tag stayed `0x000e`. Tags 2 and 3 already had their
  bodies, and the decoder already read them. No frame was added. The rules above change only
  what a recording that crosses a socket holds, and a build with the earlier rules stopped
  such a run instead of writing it. A tape replays only on the build that wrote it, so no
  older tape is read under the new rules.

A `UdpSocket` crosses under recording too, and a value holding one is cut (`HANDLE`) rather
than written. The receiving unit's reads are checked from its own stream
([Replay of a run with helpers](#replay-of-a-run-with-helpers)): the arrival's snapshot,
and the deliveries on its new ordinal.

**Takes.** A unit **takes** an arrival when it applies it: when the arrival leaves the
unit's queue of arrivals that have reached it and changes the unit's state. That happens in
`take`, in the single-item receive fast path `take_single_item`, and in `release_take`,
which takes in port order up to a retirement notice and leaves later arrivals queued.
Draining the port into that queue is not a take. Every arrival taken is one `TAKE` frame,
in the order the unit took them.

A unit takes only at a **take point**: the scheduler's pump, between tasks or at an idle
wake, and, inside an operation, each pump of `conn.send`, `conn.try_send`, `conn.receive`,
`conn.try_receive`, `conn.receive_until`, an inbox's cleaning and its seal wait, a helper's `join` or cleaning
wait, each drop of a holder counted by a charge released by a run-wide count (above), and
each drain of the unit's refund mailbox inside an operation.

The native runtime drains the mailbox at these sites, each with its kind:

| drain site | kind |
|---|---|
| the commit of every event the unit records (`record_event` in `tasks.rs`), before the event, so a take there is in the gap before that event | 0 |
| a limit's end and a limit's hand-over (`Funding::end` and `Funding::handover` in `limit.rs`) | 0 |
| a binding of a task to a worker that drains before the task resumes (`hirsel_core_task_bind` in `abi.rs`, exported, called by no emitted code; the scheduler rebinds a task through `task_resume`, which does not drain, see [limit records and hand-over](spec/budgets.md#mem-20-limit-records-and-hand-over)) | 1, a scheduler pump |
| the drop of the unit's last limit handle (`Drop for Funding`) | none: it runs after the unit's `END` is written, no admission follows it, and its folds are not takes |

Any other drain an operation makes, such as `hirsel_core_refunds_drain` (exported, emitted
by no code today), is kind 0 by the same rule. A refund take names the limit the poster refunded by its limit ordinal, with
the bytes of that one post; posts a drain folds are taken in posting order. A drain inside an operation is a take-point
position whether or not mail was pending, so the count does not depend on when other units
posted. Only a post from another unit is taken; a refund one task of the unit posts to
another task's limit in the same unit is recomputed. Several take points can
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
of the unit waits. A `TAKE` adds no event, poll or provider operation, so recording still
changes no program observation ([Laws](#laws)). Its body is one of:

| arrival | what the `TAKE` holds |
|---|---|
| item | the inbox's ordinal; the item's plain bytes and content in full; a handle snapshot for each handle it holds |
| close | the inbox's ordinal (its last client was cleaned in another unit) |
| seal answer | the ordinal of the connection whose inbox this unit cleaned. When it comes depends on how many sends other units had accepted before the seal, so the replaying unit takes the recorded answer and makes none of its own |
| settlement of a waiting send | the send's issue position in this stream; `Accepted`, `Closed` with the returned item in full, or `Withdrawn` |
| helper end | the helper handle's ordinal; the result in full with its handle snapshots, or the causes; the helper's residual steps (Step accounts, below); and each survivor charge the owner keeps ([units](spec/tasks.md#conc-15-units)): every open connection, `Shared` node, held `Secret` designation, held sealed bytes, live key object or unretired agreement registry entry ([packet ciphers and key agreement](spec/tls.md#tls-8-packet-ciphers-and-key-agreement)), and unretired `UdpSocket` pool or group queue the helper paid for, each with its ordinal and amount |
| last release | the connection's making stream and its ordinal there, its retained header and its unused room |
| shared release | the `Shared` node's home stream and its ordinal there |
| cancel | nothing: a cancellation request for this unit's root |
| refund | the limit ordinal and amount of a refund another unit posted to this unit's mailbox (a key object whose charge this unit paid and whose holder ended elsewhere, among others), or of a charge released by a run-wide count (above): a registry entry, a `Secret` designation or the sealed bytes behind one, at the paying unit's own drop or at a drain. A post that carries a UDP retirement is its `RETIRED` frame, not a refund take |

Each `TAKE` body starts with its arrival tag: item 0, close 1, seal answer 2, settlement 3,
helper end 4, last release 6, shared release 7, cancel 8, refund 9; tag 5 is unused. A
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
| `Client[T]` | the connection's identity as the inbox unit names it (that unit's stream number and its connection ordinal), its `cap` and room `b`, so a replay recomputes a send's `Bounds` failure, and its item's logical size, flat bytes and owner rows, so a replay rebuilds a connection of its layout |
| `Shared[T]` | the payload in full, and its home unit's stream number and node ordinal |
| `Socket`, `Listener`, `UdpSocket` | its direction, the bound or peer address, the negotiated protocol, peer digest and binding where they exist, and the readiness and protocol state already observed |
| a capability | its kind and its rule list in the grant snapshot encoding above (a narrowed `Net` holds its narrowed rules) |
| a capability of virtual construction | its kind and its home unit's stream number; no rule list and no cell state. Every operation on it in the receiving unit passes the replay point there (virtual capabilities across units, above) |
| `Secret` designation | its designation (below) and its canonical policy |
| `KeyShare`, `Schedule`, `Sealer`, `Opener`, `HeaderMask` | its origin and profile; a schedule's stage, generations, retired spaces and sender ledger; a sealer's floor and highest sealed counter per domain; never key bytes |
| `Args`, `Child`, `Unique`, `Site`, `StaticFn`, a plain `fn` | as plain bytes; a function as its code descriptor |

A snapshot starts with its kind tag: `Client` 0, `Shared` 1, `Socket` 2, `Listener` 3,
`UdpSocket` 4, capability 5, virtual capability 6, `Secret` 7, `KeyShare` 8, `Schedule` 9,
`Sealer` 10, `Opener` 11, `HeaderMask` 12. Any other tag is corrupt. Plain values have no
tag: the receiving type's layout says where they are. Because capabilities are plain, the
`cross` descriptor of a value lists every capability word it holds, inside list elements,
variants and `Shared` payloads included, so the recorder can snapshot each one.

A `Secret` or key object that arrives has no bytes on replay in the receiving unit, whatever
it had where it was made, so every use of it there passes the replay point with an issue
and a delivery.

Later host operations on a handle that arrived are issues in the receiving unit's stream,
checked and delivered there like any other. A replayed snapshot designates nothing on the
host, as every replayed handle does. A socket's `RETIRED` frame is in the stream of the
**refunded unit** ([UDP sockets and batches](spec/net.md#net-16-udp-sockets-and-batches)), at the
position where that unit folds it, and it names the pool by its home: the stream number
`u32` whose object ordinal it is, then the pool id (`ordinal << 1`, plus 1 for a group's
intake). That is the name its helper-end survivor carries (kinds 5 and 6 below), so a pool
an owner adopted from a helper is named the same in the owner's `RETIRED`, and replay
resolves another stream's `RETIRED` against the owner's kept survivor charge.

**Designations across units.** A designation is its origin kind (`Granted` 0, `Sealed` 1,
`Derived` 2, `Unwrapped` 3, `Fetched` 4, `GeneratedP256` 5, `Prompted` 6), the stream
number of the unit that made it, and that kind's number in that unit's own order: a grant
ordinal, the order of a seal among that unit's seals or of an unwrap among its unwraps, or
the position of the `SECRET_KDF`, credential, `MEDIA_P256_KEY` or `STDIN_SECRET` delivery. No count is run-wide. A designation that arrives keeps its origin. The canonical arguments
of `SEND_SECRET`, `SECRET_MAC` and `SECRET_KDF` bind the full designation. Every field that
holds a designation, in these arguments, `SCRAM`, `TLS_ACCEPT`, a schedule import and a
`Secret` snapshot (tag 7), writes it as origin kind `u8`, stream `u32`, ordinal `varint`.

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
and replay recomputes them. The helper's step bound is not checked: it is half the spawner's
remaining steps ([MEM-30](spec/budgets.md#mem-30-step-bounds)), which another build may count
differently ([Another build](#replay)), and the helper's stream holds the bound it received.

**Step accounts.** A same-unit spawn shares its spawner's step bound
([MEM-30](spec/budgets.md#mem-30-step-bounds)), so the order of the sharing tasks' turns
decides which of them spends the bound's last step. Each dispatch that starts or resumes a
task that shares a step bound with another live task of the unit, chosen from the ready
queue, writes a `TURN` naming that task, at the dispatch and before the task runs. A replay
starts such a task only at a due `TURN` that names it: a `TURN` that names a task the unit
does not have ready, or one that shares no bound, is `Diverged(Schedule)`, and so is a
dispatch that would start a sharing task with no `TURN` due. A task that resumes in place,
with no other task chosen between, writes none. A run with no shared bound writes no `TURN`.
A due `TURN` never pauses the running task under the FIFO chooser: the recording chose the
next task only where the running one left the processor by itself (a park, a slice end, its
end), and a replay reaches that same point and takes the `TURN` at the dispatch that follows.
A provider operation that parks with no event of its own, such as a call to a served port,
so parks first even when the `TURN` is due at its entry; a task that commits an event before
it leaves has passed the `TURN`'s position, which is `Diverged(Schedule)`.

A helper's grant is the step bound its entry holds. Its `END` and its owner's helper-end
`TAKE` both carry its **residual**, the steps of the grant it left unused (0 when it received
no bound, and for `Deadlock`). The owner returns the residual from its own `TAKE` to the bound
that granted it, at the join or the cleaning of the handle, so a replay of the owner alone
needs no other stream. A residual greater than the grant the owner's replay took is
`Diverged(Take)` at the `TAKE`. The helper's replay recomputes its residual, and its `END`
check compares it with the recorded one, as it compares the outcome.

A send is an output, so its item is checked, not stored: 16 bytes or fewer literally,
longer as a digest, as every sent buffer is. Whether a send is accepted at once, and whether
a `try_send` finds room, depends on how far another unit has received, so it is an input
and comes from the stream. A `CLOSE` of the last client or of an inbox is matched by its
ordinal and part, not by its position, as **Child processes and Unix sockets** in
[What a recording holds](#what-a-recording-holds) says for every dropped handle. A refund
a unit sends to another unit is no frame in the sender's stream: the sender's replay
recomputes it from the charges and the drops it checks, and drops it at the other unit's
mailbox as it drops published arrivals. The unit that receives it takes it as a `refund`
take.

`conn.receive`, `conn.try_receive`, `conn.new`, `conn.client`, `shared.new` and
`shared.share` are not frames. What they observe is the unit's own state after the takes
the stream supplies, so replay recomputes them: an `Empty` from `try_receive` follows from
the takes before it, and a `Capacity` from `receive` from the unit's own limit.
`conn.receive_until` uses the same takes and limit. A clock at the replay point
supplies its deadline observations and wakes as it does for `channel.receive_until`.

### Replay of a run with helpers

A replay opens every stream of the tape and starts each as its own unit, with its entry
from its stream: the root from the header, a helper from its stream's entry. It does not
wait for the parent to reach the spawn, and it may start streams in any order or in
batches, since none depends on another. A helper whose entry's code descriptor does not
resolve in the replaying build, which only `--diagnose` across revisions can meet, ends
`Diverged(Op)` at position 0 of its stream. A full replay requires a stream for every stream
number a replayed `UNIT_SPAWN` delivery names. For one the index does not hold, that unit's
verdict is `replay: Truncated at 0 in UNIT`, named from the spawner's name and spawn index,
and the spawner replays on. A stream whose parent stream is absent is named `?.<stream number>` and sorts
last. `--unit` skips the missing-stream check. Each unit replays against its own stream alone:

- every take comes from its `TAKE` frames, at their positions. Arrivals that other replaying
  units publish are dropped at the port and never taken;
- every unit operation is checked at issue and its outcome supplied, as for a provider issue;
- `UNIT_SPAWN` makes a helper handle whose end, result and causes come from the parent's
  own `TAKE` of the helper's end. It starts nothing: the child is already replaying from its
  own stream;
- `UNIT_CANCEL` and the drops of clients and inboxes are checked and reach no other unit.

So no unit's replay depends on how the others are scheduled or on how many workers run
them. A replay at any worker count of a recording made at any other worker count makes the
same observations in every unit. A program that can spawn helper units uses the
worker count in `HIRSEL_UNTIED` in ordinary runs, recording and replay. With the
variable unset it uses the tied scheduler. An invalid or zero count exits with
status 2 and names `HIRSEL_UNTIED`. Each unit waits for the others' verdicts
off its worker. `hirsel replay app --tape PATH --unit NAME` replays one
unit's stream alone and leaves the other streams unread.

**What this build replays.** Every stream replays as above, each helper as a unit of its
own from its stream's entry, on the pool beside the root or else on a thread of its own, when its entry's value, its helper ends' results, and its items hold plain bytes, lists (including lists whose elements hold handles or functions),
functions, granted capabilities, `Secret`s (other than derived or unwrapped), `KeyShare`s, `Client`s, `Shared` holders (plain payloads and locally known nested payloads), and TCP `Socket`s or `Listener`s. A helper's entry capability becomes an object of the helper's stream, by the
ordinal the entry gives it. Helper-end survivors are kept by their owner under their `SurvivorName` and released at the recorded `LastRelease`, `SharedRelease`, foreign `RETIRED`, or tag-9 `Refund` take (during replay, the limit forest holds real foreign refund posts and credits each refund at its take). Waiting sends (`CONN_SEND` `ISSUE`) enter a pending-send ledger resolved by their tag-3 settlement take (`Accepted`, `Closed`, or `Withdrawn`), returning detached crossing sockets on refusal. When every started stream has its verdict, the replay writes the
lines that are not `Sealed`, as below, and exits 1 if it wrote any, else 0. A helper end's `TAKE` that names
no helper the unit spawned, or carries a value of another layout than the handle's result
type, ends the replay `Diverged(Take)` at the `TAKE`. A kind 0 take applies at the unit's
next take point; a kind 1 take applies at the scheduler's pump, which pumps instead of
waiting while that `TAKE` is the unit's next frame, since the unit's waits for its helpers
are then not outside waits. What this build does not replay yet stops the replay with
`replay: refused: WHY` and exit 2 for the whole replay, as a refusal at open does: an entry, result or item holding a `Schedule`, a
`Sealer`, an `Opener`, a `HeaderMask`, a `UdpSocket`, a foreign `Shared` holder whose payload holds owners, or a `Secret` derived or unwrapped in another unit (its
designation does not say whether the program held its parent's bytes); and an entry, result or item holding a capability of
virtual construction or a `Net`, `Env`, `Exec` or `Cred` capability, whose rules the
root's startup holds by its own ordinals. An arriving granted capability replays as its tag 5 snapshot, numbered where it arrives and granted on the receiving unit's own cell with the kind and stored bytes the snapshot carries, so its operations are checked against that unit's stream. An arriving `Secret` replays as its designation and
policy with no bytes behind it, as one holder with no charge: its charges stay with the
unit that paid them, and only that unit's refund takes return them. An arriving `KeyShare`
replays as its snapshot, numbered where it arrives, with no key and no charge: its
agreement takes the recorded verdict, and it takes no number among the run's shares, since
the stream that made it counted it there. An arriving client replays as a stand-in: a connection of its own with the `cap`, room and item layout its snapshot carries, whose inbox no unit takes from. A send through it is checked against the stream and delivers nothing, and its drop's `CLOSE` is checked as any drop's is. A client that a replayed spawn or send hands to another unit is dropped where it leaves, so its connection's close reaches its owner, whose own `TAKE` of the close takes it. A `--diagnose` replay by a build of another revision refuses an arriving client, since the recorded layout need not be that build's. A failed or cancelled helper end delivers its recorded causes to
the join, and `UNIT_CANCEL` is checked against the owner's stream. A unit's own connections
replay: `CONN_SEND` and `CONN_TRY_SEND` are checked with the item at issue and deliver
nothing, the item reaches the inbox at its `TAKE`, and the drops of clients and inboxes are
checked as `CLOSE`s whose close, seal answer and last release are taken at their `TAKE`s. A
take naming no connection of the unit ends the replay `Diverged(Take)` at the `TAKE`. A package replay
(`hirsel replay PACKAGE`) refuses `--unit`: only the operator replay of a pinned image
replays one unit. These limits close as the helper streams' replay lands.

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
recording agree is a property of the recorder, verified by the stream-agreement tests below. The missing-stream
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

The tape format is versioned and forward-only: a reader refuses every earlier format
without migration, and a tape replays only on the build that wrote it.

The layouts below use the encodings in `crates/hirsel-tape-format`: `u8`, `u16`,
`u32` and `u64` are little-endian fixed-width integers; `varint` is canonical unsigned LEB128
(at most 10 bytes, no overlong final `0x00`, a 10th byte of at most 1); `bytes` is a `varint`
length followed by that many bytes. A field that breaks its layout, a tag outside its table,
or a byte string with bytes left after its last field is `Corrupt`.

**Blocks.** Every block, in every stream, is:

| field | width | value |
|---|---|---|
| flags | `u8` | 0, `END` 2 or `TRUNCATED` 4; any other value is `Damaged` |
| magic | 3 bytes | `HTB` |
| stream | `u32` | the stream number: 0 for the root; a helper's is allocated when its spawn starts, unique in the file, and need not be dense or in spawn order. It is not part of the unit's name |
| seq | `u64` | this block's index among its own stream's blocks, from 0 |
| len | `u32` | payload length, at most 1 MiB |
| payload | `len` bytes | |
| check | `u64` | xxh3-64, seed 0, over the 16-byte tape id, then the 20 head bytes (flags to len), then the payload |

The head is 20 bytes. The check covers the stream number, so a
block moved between streams fails its check. Each stream's block 0 is its **header block**,
with flags 0; its later blocks hold frames. The
root's header block payload holds the current format tag (`0x0012`) and the tape id every check
uses.

**Helper stream header.** A helper stream's header block payload is:

| field | width |
|---|---|
| parent | `u32`: the parent's stream number |
| spawn index | `u64`: the helper's 0-based index among its parent's `UNIT_SPAWN`s that started |
| entry | a carrier (below) holding the entry |

The header block holds the entry's first chunk. When the entry is longer than one chunk, the
stream's next blocks start with the `DATA` frames that continue it, and no other frame of the
stream comes before the entry is complete. The joined entry is:

| field | width |
|---|---|
| descriptor | `bytes`: `f`'s [code identity bytes](spec/programs.md#prog-9-code-descriptors), version byte `0x01` first |
| `n` | `u64` |
| bound | `u8` 0 when the helper received no step bound; `u8` 1 followed by the bound as a `u64` |
| `x` | a value (below) |

**Carriers.** A field that can outgrow one chunk is a **carrier**, laid out as an `Exec`
`GRANT` frame lays out its profile snapshot: a `varint` total length `T`, then a `bytes`
first chunk of 1 to `min(T, 65536)` bytes. Each `DATA` frame (head `0x09`, then a `bytes`
chunk of 1 to 65536 bytes) that follows adds its chunk, until `T` bytes are held; no other
frame comes between, and a chunk that passes `T` is `Corrupt`. The writer fills every chunk
to 64 KiB but the last. The carrier's fields are read from the joined `T` bytes, never from a
chunk. The carriers are a helper's entry, every `TAKE` body and a helper's `END` outcome. A
helper end's survivor list is inside its `TAKE` body, so it continues with the body.

**Values.** A value that crosses units, `x`, an item, a returned item, a helper's result
or a `Shared` payload, is:

| field | width |
|---|---|
| plain | `bytes`: the value's inline bytes at the receiving type's layout, with padding and every owner word (each sequence carrier, handle word, capability word and function word) written as zero bytes |
| part count | `varint` |
| parts | each a part tag `u8`, then: 0 **sequence**: `bytes`, the elements in index order at their layout, their padding and owner words zeroed; 1 **handle**: a snapshot (below); 2 **function**: `bytes`, the function's identity bytes |

Parts come in owner order: the value's owner words by ascending offset, and each sequence's
part is followed at once by the parts of the owner words inside its elements, element by
element, before the next owner word of the value that holds the sequence. Padding crosses
as zero, including any bytes outside a variant's tag and active fields. A variant is its tag,
in the plain bytes where its layout puts it, then its active case as a record of that case's
fields: each owner word of that case (a sequence, client, socket, `Shared` holder, `Secret`,
capability or function word) gets a part, and nothing of another case does. The `cross`
descriptor lists each case's owner words and padding behind a guard row that names the case
by its tag word, both inline and in sequence elements. A variant laid out in the niche of one
of its owner words keeps its case in the plain bytes: a sequence's capacity is written as
zero, which is outside its niche, and a `Shared` holder word of the case that holds it is
written as 1, the first value past the null niche, so that zero still names the other case.
A Media row names the owned word of a `KeyShare`, `Schedule`, `Sealer`, `Opener` or
`HeaderMask`. It gets one handle part, and the native word is zeroed in the inline bytes.
The format reads a
value without its type. Matching the parts to the receiving type's owners, their kinds and
each sequence's length as a multiple of its element size, is the replay's: a mismatch is
`Diverged(Take)` in a `TAKE` and `Diverged(Op)` at position 0 in an entry, as for a
descriptor that does not resolve.

A value's **canonical form** is its bytes as above with each snapshot's ordinal field left
out, at every depth. The sender cannot know the ordinals the receiving stream allocates, so
the arguments of `UNIT_SPAWN`, `CONN_SEND` and `CONN_TRY_SEND` hash the canonical form, and
stream-agreement checks compare a sent item with the canonical form of the item taken.

**Snapshots.** A snapshot is its kind tag `u8` (above), its new object ordinal in the
receiving stream as a `varint`, then:

| tag | body |
|---|---|
| 0 `Client` | inbox stream `u32`; connection ordinal `varint`; `cap` `u64`; room `b` `u64`; item logical size `u64`; item flat bytes `u64`; owner row count `varint`, then per row its offset, kind, length and stride as 4 `varint`s |
| 1 `Shared` | home stream `u32`; node ordinal `varint`; the payload as a value |
| 2 `Socket` | direction `u8` (0 made by `CONNECT`, 1 by `ACCEPT`); peer address, 19 bytes as a `LISTEN` delivery writes one; TLS `u8` 0, or 1 followed by the `TLS_CONNECT` success payload as `bytes`; state `u8`, bit 0 end of stream received and bit 1 reset received, as the sending unit's stream showed them (sockets that cross, above), other bits `Corrupt`; peer address for `CONNECT` the address it named |
| 3 `Listener` | direction `u8`, always 0, any other value is `Corrupt`; bound address, 19 bytes |
| 4 `UdpSocket` | direction `u8`, always 0, any other value is `Corrupt`; bound address, 19 bytes; peer `u8` 0, or 1 followed by the connected address, 19 bytes |
| 5 capability | grant kind `u8` in `GrantKind`'s numbering (`Dir` 0, `Net` 1, `Clock` 2, `Random` 3, `Stdout` 4, `Stderr` 5, `Stdin` 6, `Env` 7, `Process` 8, `Tasks` 9, `Exec` 13, `Cred` 14; a `random_tape` cell is `Random`), as its `GRANT` frame writes it; then `bytes`: for `Net` its rule count `varint` and rules as the header's grant snapshot writes them after the `Net` kind byte; for `Env` its record as the header writes it after the `Env` kind byte; for `Cred` its record after its kind byte; for `Exec` the profile snapshot; for `Stdin`, `Stdout` and `Stderr` the terminal byte; for every other kind nothing |
| 6 virtual capability | grant kind `u8`, as for tag 5; home stream `u32` |
| 7 `Secret` | designation: origin kind `u8`, stream `u32`, ordinal `varint`; canonical policy text `bytes` |
| 8 `KeyShare` | media origin (below); group `u8` (`X25519` 0, `P256` 1) |
| 9 `Schedule` | media origin; profile; side `u8` (`Client` 0, `Server` 1, imported send 2, imported receive 3); stage `u8` (handshake 0, `Application` 1); send generation `u64`; receive generation `u64`; retired spaces `u8`, bit `i` for space `i`; exported `u8` (1 once `srtp` was called on it); ledger count `varint`, then per issued domain of a current send epoch its space `u8` and domain `u64`, sorted by space then domain. An SFrame sending schedule continues with the first KID `u64` and the width of the KID block in bits `u8` |
| 10 `Sealer` | media origin; profile; space `u8`; domain `u64`; `floor` `u64`; sealed `u8` 0, or 1 followed by the highest sealed counter `u64` |
| 11 `Opener` | media origin; profile; space `u8` |
| 12 `HeaderMask` | media origin; profile; space `u8`; `local` `u8` |

A **profile** is two bytes: the family `u8` (`Quic` 0, `Dtls13` 1, `Tls13` 2,
`QuicMultipath` 3, `dtls12` 4, SRTP 5, `quic_initial` 6, SFrame 7), then the suite `u8`, the case's
index in its declared order in `Suite13`, `Suite12`, `SrtpProfile` or `SframeSuite`, and 0 for
`quic_initial`. A space is its case index in `Space`'s declared order (`Initial` 0 to
`Rtcp` 4). The stage is 0 for Initial and handshake schedules and 1 once the application
space is installed, or for SRTP and SFrame. SFrame uses the Application space and uses its
KID as the ledger domain and the sealer's domain. Its generation advances by the number
of ratchet steps, in the imported direction. The sending block is part of the schedule
snapshot because it determines which KIDs the receiver can issue.

A **media origin** is the creating stream `u32` followed by its delivery position
`varint`, counted as verdicts count it. If there was no delivery, position zero is
followed by the primary kind 3 charge ordinal `varint`. This names a virtual Random's
`KeyShare`, a `quic_initial` schedule and other locally derived objects without a made-up
delivery. For a sealer, the primary charge is the cipher charge. The object's origin
and the names of its charges remain the same across crossings.

An arriving media object gets a new object ordinal in the receiving stream. Subsequent
media capture arguments refer to it as the ordinal `u64` followed by byte 3, disjoint
from a delivery position and the two members of a sealer pair. An agreement's share
operand uses origin kind 2 and the receiving ordinal `u64`. This applies even if the
object was constructed virtually. A refused send keeps its sending unit's name. A key object made outside a recorded unit has no origin to write and cuts the recording that encounters it.

The format tag is `0x0012` because a helper's `END` and its owner's helper-end `TAKE` carry
the helper's residual steps, and a `TURN` frame records each turn of a task that shares a step
bound (format `0x0011` gave a `Client` snapshot its connection's item layout, which an arriving
client's replay rebuilds; format `0x0010` gave a `Fail` cause its message's
capacity, which the join charges; format `0x000f` gave the media origin a charge ordinal and the SFrame
schedule snapshot its sending block).

A capability (tag 5) that arrives gets its new ordinal in the receiving unit, and its later
operations there belong to that unit, on that ordinal. If the same word arrives again, the
latest ordinal is the one used. An `Env` is read through the run's own grant wherever it is
used, so it gets an ordinal but no operation names it. A `Cred` is rebound to the receiving
unit's ordinal, so a fetch through it is written in that unit's stream. A `Net` that arrives
on replay checks its operations against the rules its snapshot holds. A narrowed `Exec`,
whose profile no snapshot holds, is a capability of virtual construction (tag 6) whose home
is the unit that narrowed it; a unit it reaches keeps that home when it sends it on. The
recording unit that takes it gets its profile from the cell that recorded the narrowing,
so its spawns and further narrowings there are checked against the narrowed profile; a
`SPAWN` through it is an issue and a delivery as through any `Exec`. A narrowing is
recomputed, not written, and tag 6 holds no profile, so replay refuses an arriving
capability of virtual construction. A value holding any other capability of virtual
construction still ends the stream at that value.

**`TAKE` frames.** `TAKE` is frame kind 14 (kind 10 is `END`), and its head is `0x0E`: the
task bit is 0, because a take names no task. Then:

| field | width |
|---|---|
| gap | `varint`: events since the previous frame, as a `WAKE`'s |
| point | `u8`: 0 an in-operation take point, 1 a scheduler pump |
| count | `varint`: the take point's count |
| body | a carrier holding the arrival tag `u8` and the fields below |

| tag | arrival | fields after the tag |
|---|---|---|
| 0 | item | inbox ordinal `varint`; the item as a value |
| 1 | close | inbox ordinal `varint` |
| 2 | seal answer | connection ordinal `varint` |
| 3 | settlement | the send's `ISSUE` position `varint`; outcome `u8`: 0 `Accepted` and nothing, 1 `Closed` and the returned item as a value, 2 `Withdrawn` and nothing |
| 4 | helper end | helper ordinal `varint`; outcome `u8`: 0 a result and the result as a value, 1 `Failed` and its causes, 2 `Cancelled` and its causes; residual `varint`, at most the grant the owner took; survivor count `varint`, then the survivors in ascending (kind, origin kind, stream, ordinal, amount) order |
| 6 | last release | making stream `u32`; connection ordinal `varint` in that stream; retained header `u64`; unused room `u64` |
| 7 | shared release | home stream `u32`; node ordinal `varint` |
| 8 | cancel | nothing |
| 9 | refund | limit ordinal `varint`; amount `u64` |

- **Causes** are a count `varint` of at least 1, then each cause of the chain, primary
  first: its kind `u8` in `CauseKind`'s declared order (`Overflow` 0 to `Stack` 8), its
  site `u64`, then for `Fail` its message as `bytes` and the message's `String` capacity
  as a `varint` (0 for static text, which no limit pays for; the owner's join admits a
  message of that capacity, so its replay charges what the run did), for `Capacity` its requested and
  available bytes as two `u64`s, and nothing for the others.
- A **survivor** is its kind `u8` (connection 0, `Shared` node 1, `Secret` designation 2,
  key object 3, agreement registry entry 4, `UdpSocket` pool 5, group queue 6, sealed bytes
  7), then for kinds 2 and 7 the designation's origin kind `u8`, then the stream `u32` that
  names it, its ordinal `varint` and its amount `u64`. The stream is the one that made the
  survivor, or its home: for kinds 0, 1, 5 and 6 the stream whose object ordinal it is, for
  kinds 2 and 7 the designation's stream, and for kinds 3 and 4 the stream of the unit that
  admitted the charge. A helper passes up charges it kept from its own helpers, so that
  stream need not be the helper's. The ordinal is the object ordinal in that stream, for
  kinds 2 and 7 the designation's ordinal there, and for kinds 3 and 4 the charge's number
  among the key object and registry entry charges that unit admitted, from 0 in its own
  order. Each charge of [packet ciphers and key
  agreement](spec/tls.md#tls-8-packet-ciphers-and-key-agreement)'s table is numbered, so a
  sealer's cipher and domain ledger are two names, and a charge admitted for a construction
  later refused takes its number too. The count is taken in record and replay alike, as a
  unit's seal count is. A delivery position would not do: a key object of a virtual
  `Random`, a `quic_initial` schedule and every object derived from a schedule of one have
  no delivery, and one delivery can admit several charges. A later `last release`, `shared
  release`, `RETIRED` or refund then names one survivor, however many helper levels it
  passed through. Any other kind is `Corrupt`. The owner that keeps a kind 2, 4 or 7
  survivor is its paying unit from then on, and takes its refund as above. Every designation
  narrowed from sealed bytes keeps their origin, so two narrowed designations of the same
  bytes are two kind 2 survivors with one name, each with its own amount. A kind 5 survivor
  is a socket's pool (a group's intake included) and a kind 6 survivor a group member's
  queue; the owner that keeps one records its later `RETIRED`. A survivor lists only the
  charges the owner took over at the end take: one released between the helper's end and the
  take is not a survivor.
- A settlement's position must name a `CONN_SEND` `ISSUE` of this stream that is still
  outstanding. The settlement completes it, as a `DONE` would, and its task may issue
  again. Any other position is `Corrupt`.

**`TURN` frames.** `TURN` is frame kind 15. Its head is `0x1F` (kind 15 with the task bit),
then the gap `varint`, as a `WAKE`'s, and the task `varint`: the stable unit-local identity
of the task the dispatch starts. A `TURN` adds no scheduler event.

**Unit operations.** `UNIT_SPAWN`, `CONN_SEND`, `CONN_TRY_SEND` and `UNIT_CANCEL` are
operation codes 164, 165, 166 and 167 of `ISSUE`, `DONE` and `OP`, in
`hirsel_core_ids::host_op`, whose table refuses a duplicate code (162 and 163 are
`SCHEDULE_SPACE_INFO` and `SCHEDULE_SEALER_PAIR`). Their frames use the standard `ISSUE` and
`OP` encoding, task bit included. Their `Out` holds only the status, which is `OK` (0), and the
`value` `varint`: no error, written count or payload. Any other status or value is
`Corrupt`.

| code | operation | frame | object | args | `Out` value |
|---|---|---|---|---|---|
| 164 | `UNIT_SPAWN` | `OP` | 0 | `Args` over the descriptor `bytes`, `n` as a `u64` and `x` in canonical form | the helper's stream number |
| 165 | `CONN_SEND` | `OP`, or `ISSUE` for a send that waits | client ordinal | `Args` over the item in canonical form | 0 `Accepted`, 1 `Closed` |
| 166 | `CONN_TRY_SEND` | `OP` | client ordinal | as 165 | 0 `Accepted`, 1 `Full`, 2 `Closed` |
| 167 | `UNIT_CANCEL` | `OP` | the helper handle's ordinal | the empty literal | 0 |

A served port's `PORT_NEXT` is always an `ISSUE` and a `DONE`. Its `DONE` holds status `OK`
and `written` `varint`: 0 for a `next` answered `None` at quiescence, with `value` 0 and
total length 0; 1 for a call or a released notice, with the call id as `value` and, as its
payload, the nine call words as `u64`s then the call's whole payload. Any other `written`,
or a `written` 1 payload shorter than 72 bytes, is `Corrupt`. `CALL_WORD`, `CALL_PAYLOAD`
and `CALL_PAYLOAD_PREFIX` are no frames and are answered from it. `PORT_REPLY` and a port
fill are `OP`s whose `Args` cover the reply word and bytes; `CLOSE_SERVED` and `CLOSE_CALL`
are checked as `CLOSE` is. The `PORT_NEXT` payload is its total
length `varint`, then the first `bytes` chunk, then `DATA` continuations as for a carrier.
The first chunk is empty only when the total length is 0. A nonzero total length with an
empty first chunk is `Corrupt` in its stream.

**`END`.** The root stream's `END` uses the root outcome layout. A helper stream's `END` (any stream number
other than 0) is head `0x0A`, the gap `varint`, the status `u8` 0, the kind `u8`
(`Returned` 0, `Failed` 1, `Cancelled` 2, `Deadlock` 3), then a carrier holding, for
`Returned`, the result in canonical form (the owner's helper-end take holds it as a
value, with the owner's ordinals); for `Failed` and `Cancelled`, the causes; for `Deadlock`,
the event count `varint`; and after it, in the same carrier, the residual `varint`. In either kind of stream, the block that holds the frame
completing the `END`, the `END` itself or its last `DATA` frame, is flagged `END` and holds
no frame after it.

**The index.** Blocks of different streams interleave in the file in whatever order the
writer took them. A replay indexes the file in one sequential pass at open and then reads
each stream from its own blocks. Discovery never resynchronises. The reader numbers blocks
`B` = 0, 1, 2, … in file order, across all streams; `B` is the number in
`replay: Damaged at block B`: B counts the complete blocks before the faulty one, so a
trailing partial block is numbered by the complete blocks before it. Block 0 must be stream
0's header block, with flags 0; anything else there is `Damaged` at 0. Each later block is
read whole and its head and check verified; then:

- a block whose stream number is not yet indexed and whose sequence number is 0 opens a new
  stream, with a helper stream header and flags 0;
- a block of an indexed stream that has not ended, with that stream's next sequence number,
  continues that stream;
- anything else is `Damaged` at `B`. By name:
  - a failed check, a length over 1 MiB, a bad magic, a flags byte other than 0, `END`
    alone or `TRUNCATED` alone, or a `TRUNCATED` block with a non-empty payload;
  - a repeated sequence number: a block of an indexed stream whose number that stream already
    holds, a second header for one stream number included, stream 0's among them;
  - reordered blocks: when two blocks of one stream are swapped, the first of them in file
    order skips its stream's next number, so it is `Damaged` as a sequence gap, at its own B;
  - a sequence gap: a number above the stream's next;
  - a block of a stream that already ended, with `END` or `TRUNCATED`;
  - a helper stream's block before its header: the first block of a stream number not yet
    indexed whose sequence number is not 0;
  - a helper stream header block whose payload is shorter than the parent and spawn
    index (12 bytes). A fault in its entry carrier is `Corrupt` at 0 in that stream;
  - a helper stream header whose parent is its own stream number, or whose parent and spawn
    index equal those of a stream already indexed: the two would get one unit name.

So a stream may begin anywhere in the file before the root's `END` block, and the reader
tells a new stream from trailing bytes by these tests alone, never by position. A parent
may be indexed later than its child. A parent chain that returns to a stream already on
it is `Damaged`, at the `B` of the header of the lowest-numbered stream of the cycle, when
the pass finds no other `Damaged`; with several cycles, the least such `B`. A chain that ends at an absent stream is an orphan,
not damage.

The writer writes the block flagged `END` of stream 0 last, after every other stream's
blocks, so a partial block can follow every `END` only in a damaged file.

At the end of the file, a trailing partial block is `Truncated` for each stream without
`END` when stream 0 has no `END`. Any block or byte after stream 0's `END` block, a partial
block included, is `Damaged` at its block number. A `Damaged` index writes
its one file line and exits 1, whatever the stream verdicts. A `TRUNCATED` block, with an
empty payload, ends its own stream `Truncated` and no other. Each stream is decoded with its
own state: its previous task, its outstanding issues and its open carrier.

`tape show` prints the header, then each stream under its unit name, ordered by
spawn-index path, compared numerically.

**Example.** A root-only tape with one `TAKE`: a refund of 48 bytes to limit
ordinal 0 (the root limit), taken at a scheduler pump with gap 0 and count 0, and then an
`END` returning exit status 0. The tape id is the bytes `00` to `0f`. The header has an
empty identity, a zero revision, charge policy 0, limit 65536, bound 0, the FIFO chooser,
an empty `arg` and no grants. A recorder writes the run's launch metadata in `arg`; the
example leaves it empty to stay short. Checks are xxh3-64 with seed 0, written little-endian.

Block 0 (stream 0, seq 0, flags 0, 75-byte payload), 103 bytes:

```text
00                         flags
48 54 42                   magic "HTB"
00 00 00 00                stream 0
00 00 00 00 00 00 00 00    seq 0
4b 00 00 00                len 75
12 00                      format version
01                         kind Run
00 01 02 03 04 05 06 07 08 09 0a 0b 0c 0d 0e 0f   tape id
00                         identity: empty bytes
00 × 32                    revision
00 00 00 00                charge policy 0
00 00 01 00 00 00 00 00    limit 65536
00 00 00 00 00 00 00 00    bound 0
00                         chooser FIFO
00                         arg: empty bytes
00                         grants: empty bytes
dc d0 35 8d 4b 5f eb 19    check
```

Block 1 (stream 0, seq 1, flags `END`, 25-byte payload), 53 bytes:

```text
02                         flags END
48 54 42                   magic "HTB"
00 00 00 00                stream 0
01 00 00 00 00 00 00 00    seq 1
19 00 00 00                len 25
0e                         TAKE (kind 14, task bit 0)
00                         gap 0
01                         point kind 1: scheduler pump
00                         count 0
0a                         body carrier: total length 10
0a                         first chunk: 10 bytes
09                           arrival tag 9: refund
00                           limit ordinal 0
30 00 00 00 00 00 00 00      amount 48
0a                         END (kind 10)
00                         gap 0
00                         status complete
00                         kind Returned
04 00 00 00 00             outcome: literal args, 4 bytes, exit status 0 as u32
91 50 4f bc 0f 1f 01 54    check
```

### Prior art

| system | what it does for many actors | taken? |
|---|---|---|
| rr (`b3e8e72`) | One trace per recording with substreams (`events`, `raw data`, `mmaps`, `tasks`), and every event frame tagged with its thread id and a single global time. That one order exists because rr runs one thread at a time while recording, and replay follows its recorded decisions | **ADAPTED.** One file holding several streams is taken, as are tagged frames. The single global order is **NOT TAKEN**: it serializes every thread through one core while recording, which would undo the point of units, and Hirsel has no cross-unit event order to record. Weakness avoided: recording cost grows with parallelism. Win taken: one artifact per recording. |
| TigerBeetle VOPR (`a2a3c70`) | Records nothing. The whole cluster, replicas and network alike, is a deterministic function of one seed, and the network is a simulator drawing loss, replay and partitions from that seed's PRNG | **NOT TAKEN** for recordings: a live Hirsel run's inputs come from the host, so they cannot be regenerated from a seed. It stays the model for `hirsel test --sim`, which already plays that part. Win noted: replaying a single actor needs only its own inputs, which is what per-unit streams give. |
| Antithesis-style deterministic hypervisor (public documentation only, no source) | Runs the whole system on one deterministic machine and replays it from its seed and injected inputs, so every actor shares one order | **NOT TAKEN**, for the same reason as rr's global order: it needs a whole-machine single order, which units do not have and must not acquire. |
| Goose (`d5ae5bb`) | Threads share no mutable memory and meet only through typed queues of flat values; it has no record or replay | **ADOPTED** as confirmation of the input model: when actors meet only through queues, each actor's queue takes are its whole outside input, and that is the `TAKE` frame. |

### Unit-recording verification properties

Each verification property runs in an existing harness: the tape tests (`crates/hirsel-core-rt/src/tape/tests.rs`)
for frames, the unit ledger trace (`crates/hirsel-core-rt/src/units_ledger_law.rs`) for
take order, and `hirsel test --replay` for whole programs.

| property | bug caught | verification setup |
|---|---|---|
| **Stream framing and corruption** | a stream number outside the block check, a `TAKE` that decodes to another arrival, or an earlier-format tape admitted | Hand-built byte strings verify that changing a block's stream number fails its check, each `TAKE` body decodes to its expected arrival, earlier-format tapes are refused at open, structural errors (sequence gaps, duplicate headers, repeated or swapped blocks, post-`END` blocks, missing helper headers, duplicate `(parent, spawn index)`, parent cycles, flipped bytes, trailing bytes) report `Damaged` at the exact block, and invalid tags or payloads report `Corrupt`. |
| **Per-unit standalone replay** | a helper's input missing from its stream (an unrecorded take path, or an entry stored only as a digest) | A program whose root spawns two helpers exchanging items over connections (with host randomness and crossed virtual capabilities) is recorded and each unit is replayed alone with `--unit` and every other helper stream removed. Each replay ends `Sealed` with zero host provider calls and matching outputs. |
| **Exact take positions** | a take applied at another point in the unit's order than recorded | Scripted delivery positions verify that recorded `TAKE` gaps, point kinds (in-operation vs scheduler pump), and counts match hand-counted positions across `try_receive`, parked `receive`, `release_take`, `Shared` snapshots, and `shared release` takes on both Cranelift and LLVM. |
| **Cross-stream agreement** | a recorder writing a sent item differently from what the receiver took, or a helper entry unlike its spawn | A test reader pairs every `CONN_SEND` with the `TAKE` it reached and every `UNIT_SPAWN` with its child's entry, checking that canonical arguments, `Shared` snapshots, survivor lists, settlements, and crossed capabilities (including granted and narrowed `Net` and `Env`, and `Cred`) agree across streams, and that a `Cred` moved into a helper is fetched there on the helper's own ordinal. |
| **Worker-count independence** | a replay depending on scheduling across units | Recording at `HIRSEL_UNTIED=1` and `HIRSEL_UNTIED=8` and replaying each tape at both worker counts yields `Sealed` with identical outputs. |
| **Per-helper divergence reporting** | a divergence stopping other units, going unreported, or printing in timing-dependent order | Replaying with edited helper code verifies that each diverged helper reports `Diverged(Arg)` or `Diverged(Take)` at its exact position in deterministic unit-name order while other streams still finish `Sealed`, and a missing helper stream reports `Truncated at 0 in UNIT`. |
| **Zero overhead on observations** | unit recording adding a charge, step, poll, or event | Running a multi-unit program across an uninstrumented build, a recording build with recording off, and a recording build with recording on produces identical outputs, `Capacity` refusals, step counts per unit, and provider call logs. |

## Format

A recording file is a **tape** owned by `crates/hirsel-tape-format`. The current tape layout
includes per-stream blocks, terminal flags on stream grants, SCRAM frames
([SCRAM-SHA-256](spec/tls.md#tls-6-scram-sha-256)), canonical `Secret` grant policies
([secrets](spec/tls.md#tls-5-secrets)), `SEND_SECRET` designation lists, `Cred` grant and
fetch frames ([credential grants](spec/tls.md#tls-7-credential-grants)), and the 32-byte
build digest at the end of launch metadata. A replay refuses tapes written by earlier
formats. The format is not part of the language and a replay reads only tapes its own build
admits; its pinned executable reads that layout, and the host uses the checked block-zero
prefix through the launch arguments to find and verify the pin.

- The header holds no Map hash key. Every run, a replay included, draws its own key from
  host entropy at entry, and no observable depends on it
  ([maps](spec/semantics.md#sem-5-maps), [the replay point](spec/capabilities.md#cap-8-the-replay-point)),
  so a replay supplies nothing for it and compares nothing with it.
- The header has no mode byte. Its grant snapshot stores the run's root Net rules in order
  and with duplicates; each address rule occupies 23 bytes, preceded by the byte 8 when
  its reach is `Public` ([public-only connects](spec/net.md#net-18-public-only-connects)).
- The `Tls` rule record holds the direction byte 2; the trust byte, followed by 32 bytes of
  roots digest when the trust is `Roots`; the port interval; and a `u16` count of patterns,
  each a length byte followed by its bytes.
- A `Secret` grant record holds the parameter name, the source label and the canonical
  policy text ([secrets](spec/tls.md#tls-5-secrets)), all length-prefixed.
- A `Cred` grant record, after the `Secret` grant records, holds the parameter name, the
  source text and the canonical policy text
  ([credential grants](spec/tls.md#tls-7-credential-grants)), all length-prefixed. A fetch's issue holds
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
([provider operations](spec/capabilities.md#cap-5-provider-operations)).

## Laws

| property | bug caught | verification setup |
|---|---|---|
| **Recording changes nothing** | recording machinery that adds a charge, a step, a poll or a second read, or leaks into a build without recording | For a corpus of small programs with scripted providers and ordered senders, run the build without recording, the recording build with recording off and the same build with recording on, on both engines. Their outputs, outcomes, cause sites and chains, `Capacity` refusals, step counts and provider call logs are equal. The executable built without recording defines no symbol of the recording runtime. |
| **Divergence ends replay at its position** | a replay that continues past a mismatch, or reports it to the program | Record a run, then replay it with the program edited to keep a dropped handle open, omit its last send, send different bytes, or read into a buffer too small for a recorded delivery. Each replay ends `Diverged` at the exact frame position, exits 1, and records zero provider calls. Replays that drop a handle at a different point or read into a larger buffer still end `Sealed`. |
| **Replay is offline** | replay reaching the host or needing keys | Record an HTTPS GET against a loopback server. Replay it with a refuse-all inner provider, the server stopped, and the roots file removed. The response bytes, the protocol and the peer digest equal the recorded run's, and the inner provider records zero calls. |
| **Replay without the secret** | replay reading the secret's source | Replay a recorded run that sent a `Secret` ([TLS verification](spec/tls.md#laws)) with the environment variable unset. The run starts, its outputs match byte for byte, and the inner provider records zero calls. |
| **Secrets never reach a recording** | a secret's bytes appearing in a frame or digest | Search a recording of a `Secret`-sending run for the secret value, its base64 encoding, and its raw bytes: there are no hits. |
