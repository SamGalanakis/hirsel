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
| `SECRET_MAC` key operations ([TLS-5](spec/tls.md#tls-5-secrets)) | object 0; mode 2 `wrap`, 3 `unwrap`, 4 `sign`; canonical args = mode, the key's designation and policy, then for `wrap` the wrapped secret's designation and policy, for `unwrap` the `uses` text and the blob, for `sign` the algorithm and `msg`, and the spare of `out` | the outcome; for `wrap` and `sign` on success the blob or signature. `Capacity`'s available bytes are the recorded spare. |
| `SECRET_MAC` AEAD ([TLS-5](spec/tls.md#tls-5-secrets)) | object 0; mode 5 `aead_seal`, 6 `aead_open`; canonical args = mode, key designation and policy, algorithm, nonce, associated data, plaintext or sealed bytes, and output spare | the refusal or the appended bytes, including the authenticated plaintext for open |
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
unwraps, and a derived one by the position of its `SECRET_KDF` delivery, each together
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
