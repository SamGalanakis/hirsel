# Fleet

*Specified 2026-11-26 (converged Hirsel Fleet blueprint, DECISIONS.md). Rules FLT-1 to FLT-8,
kernel rules K1 to K13, directory rules D1 to D6, and verification tiers T0 to T3.
Phase 1 builds the move-authorization kernel, independent register model, simulation laws
and formal proofs. Phase 3a adds the dedicated `Directory[K]` Place library and
`hirsel fleet sim` at 50 and 1,000 simulated hosts. The [launcher](launch.md#the-launcher)
carries opaque move envelopes and the release embeds `platform_main`. Real-cluster
orchestration verbs, Store `ClaimAuth`/`ORIGIN`, Runner `Deposed` and the platform move
integration remain planned.*

Hirsel Fleet runs multi-machine workloads with the existing `hirsel` launcher and `platform`
host packaged as one single-binary distribution, with no Kubernetes, no second host daemon,
no embedded Raft beside Store and no gossip mesh. The control plane `fleet@1` is an ordinary
standalone durable `Place` on a conditional-write object bucket (`objects@1`). When `fleet@1`
is unreachable, already-placed workloads continue serving and healthy incumbents continue
restarting locally; only new cross-host grants pause.

## FLT-1: Three workload archetypes

Every Fleet workload maps to one of three archetypes, separated strictly by what fences its
authority:

| Archetype | Workloads | Safety fence | Clock in safety? | Failover / move |
|---|---|---|---|---|
| **A · Durable `Place`** | Databases, sharded KV (`cluster@1`), durable workflows, `fleet@1`, `Directory[K]` | `objects@1` head CAS (`ClaimAuth` + `ORIGIN` witness) plus the workload's own in-Place fence | **Never** (`STORE-SPEC`) | Whole-Place move via conditioned claim (planned: warm follower; crash: direct claim) |
| **B · Ephemeral Stateful Unit** | `sfu_cascade` media rooms, volatile S3-FIFO caches (`cache@1`), pub/sub relays | Dedicated `Directory[K]` Place(s): term CAS + drift-bounded lease `W = ceil(L_max * (1 + d) / (1 - d))` | **Yes** — suspend-inclusive elapsed clock (`CLOCK_BOOTTIME` / `mach_continuous_time`) | State reset by contract; new term waits `W` after directory takeover |
| **C · Elastic Remote `Compute`** | Ray/FLAME-style tasks, shuffles, batch and AI jobs, scheduled by the reusable core scheduler (`compute@1`, [compute.md](compute.md)) | Worker-only admission; three retry classes: `pure` ([SEM-8](semantics.md#sem-8-determinism)), `keyed` (receiver-checked idempotency), and `effectful` (inert create, pinned activation, never re-dispatched) | **Never** | Dispatch `(release_id, entry_name, codec_args, bounds)` directly to an admitting worker via [compute.md](compute.md) (never placed by `fleet@1`; never ship functions, [PROG-9](programs.md#prog-9-code-descriptors)) |

## FLT-2: Move protocol (`ClaimAuth`, `ORIGIN`, `Superseded`, settlement, `Deposed`)

### Universal `ClaimAuth`

Every non-genesis `objects@1` writer open (moves, local restarts, installs, serving
candidates, attached-stream janitors and explicit repairs) carries a `ClaimAuth` precondition
in `store.Options.claim_auth` (STORE-SPEC §12.3 "ClaimAuth"):

```text
pub type Claim: codec + key + shareable + cross { epoch: u64, holder: Bytes }   // holder: 16 bytes
pub type ClaimClass: codec + key + shareable + cross = Permit | Continue | Adopt
pub type ClaimAuth: codec + key + shareable + cross { expect: Option[Claim], attempt: Bytes, class: ClaimClass }   // attempt: 16 bytes
```

- **Checked before any write.** The store checks `expect` against the head's `(epoch, holder)`
  on every iteration of the open, before any write, including before any attached-stream
  `Held` row (§12.11). On mismatch it returns `Superseded(actual)` and writes nothing. A missing
  or malformed auth is `Missing("claim_auth")`; there is no default.
- **`Superseded` names the successor.** Whenever an open finds the head claimed away from its
  pair, including on the attached path where it would otherwise return `Attached`, the answer
  is `Superseded(actual)` for a validated successor (the same Store at this namespace).
- **Genesis and Adopt.** Genesis takes only `expect: None`. `expect: None` is otherwise
  permitted only with `class = Adopt` from an explicit local operator action on a namespace no
  Fleet manages, never from a restart, wake, retry, janitor or remote message. A Runner move
  envelope that carries `Adopt` is refused (RUNNER-SPEC §14.6). An Adopt is pinned to the first
  pair it admits for the rest of that open.
- **Fresh random `holder` per physical open.** Every physical open draws a fresh, private
  random 16-byte `holder`. `attempt` is never used as the holder: `history_write_head` resolves
  `Mismatch` by byte equality, so identical proposals from two physical opens would both be
  `Applied` and create dual writers.

### The `ORIGIN` head frame

The claim's head replace (`H`) writes a 49-byte `ORIGIN` frame (kind `0x00E0`, `MUST = 1`)
atomically with the new `(epoch, holder)` pair. It is part of Store Format 11.

```text
pub type Origin: codec + key + shareable + cross {
    epoch: u64, attempt: Bytes, class: ClaimClass, permit_epoch: u64, permit_attempt: Bytes
}
```

Wire payload layout (49 bytes, little-endian):

- `epoch: u64`: the epoch installed by this claim; equals `HEAD.epoch`.
- `attempt: [16]`: this claim's attempt identifier.
- `class: u8`: `0 = Permit` (Fleet move), `1 = Continue` (same host: restart, install
  candidate, janitor), `2 = Adopt` (explicit operator open).
- `permit_epoch: u64`: the epoch installed by the latest `Permit` claim in this head's lineage;
  `0` if none.
- `permit_attempt: [16]`: that `Permit`'s attempt; all zero if none.

A `Permit` claim sets the permit fields to its own `epoch` and `attempt`; a `Continue` or
`Adopt` claim copies them from the head it replaces. Every same-epoch head write (advance,
`CLAIM`, cut, settle, direct service, activation install) copies `ORIGIN` byte for byte.
Decoding rules (any violation is `Corrupt`):

- **O1.** `ORIGIN` is present exactly when `HEAD.epoch >= 2`; the genesis head has none.
- **O2.** `MUST = 1` and the payload is exactly 49 bytes.
- **O3.** `ORIGIN.epoch == HEAD.epoch`.
- **O4.** `class <= 2`; a `Permit` has `permit_epoch == epoch` and `permit_attempt == attempt`;
  a `Continue` or `Adopt` has `permit_epoch < epoch`, with an all-zero `permit_attempt` when
  `permit_epoch == 0` and `permit_epoch >= 2` otherwise.

The format change is one-way: a Format 10 binary refuses a Format 11 store `Unsupported(11)`,
and a Format 11 binary refuses a Format 10 store `Unsupported(10)`.

### Read-only settlement and Rule F1

Fleet persists a write-ahead permit row `X { expect: (e, h_A), attempt, target: B }` before
releasing any move command. It settles `X` with `store.settle_permit` (STORE-SPEC §12.3
"HeadView and Permit settlement"), which makes one **fresh** GET of the Place's head through
Fleet's own trusted client, checks that the head is the expected Store at the expected
namespace, and writes nothing:

```text
pub type HeadSettlement: codec + key + shareable + cross = HeadOpen | HeadWon(u64) | HeadDead(Claim)
pub fn settle_permit(mut client: std.object.Client, store: StoreId, predecessor: Claim, attempt: Bytes, op_ms: u64) -> Result[HeadSettlement, Error] waits
```

| Fresh head read | Settlement | Controller action |
|---|---|---|
| `pair == (e, h_A)` | **`HeadOpen`** | `X`'s CAS may still apply. Reading the old pair never cancels `X`. To force progress against a dead target, authorize one bounded recovery contender sharing the same `expect`. |
| `HEAD.epoch <= e` and `pair != (e, h_A)` | **error** (`Corrupt(Regressed)`) | A regressed or sideways head proves nothing about `X`. Close nothing; alert. |
| `origin.permit_epoch == e + 1` and `origin.permit_attempt == X.attempt` | **`HeadWon(e + 1)`** | Close `X` as won. `HEAD.epoch - (e + 1)` counts `B`'s later local `Continue` claims. Any later move names the head's current pair. |
| otherwise (`HEAD.epoch > e`) | **`HeadDead(pair)`** | Epoch `e + 1` went to another claim (`Continue` on `A` or a recovery contender); `X` can never apply. Close `X` as superseded and never resend it. |

- A foreign Store or another namespace is an error and closes nothing. A cached head, a
  follower's view or a caller-supplied `HeadView` never settles a permit.
- `HeadWon` describes a historical claim. It confers no present writer authority: `B` (or its
  successor) restarts with `Continue(expect = the current pair)`.
- `X.attempt` must be unique per permit; Fleet draws it at random when it writes `X`.
- **Rule F1 (single predecessor per open set).** The control transaction that authorizes a
  `Permit` on Place `P` requires that every open attempt on `P` shares the new attempt's
  `expect`. While `X` is open, only `X` and a recovery contender with the same `expect` can
  install a new `Permit`. Fleet settles `X` before authorizing any move from the successor
  pair, and acts on each settlement once.

### Deposed end cause and the three stale-open races

Runner adds one end cause, `Deposed(store.Claim)`, answered to the host as
`PlaceDeposed(epoch, holder)`. Only an open refused `Superseded(actual)` ends `Deposed(actual)`,
because only it names the successor. A running writer's `Fenced(e)` names no holder and stays an
ordinary error; the host's next launch with the same auth is refused `Superseded(actual)` before
any write and ends `Deposed`. The platform parks the assignment on `PlaceDeposed` and starts no
restart. Even if the end report is lost, a retry still names the old `expect` and is refused
again.

Together, `ClaimAuth`, `ORIGIN` and `Deposed` close all three stale-open races and the lost-`H`
window:

1. **Delayed `A -> B` move after `C` wins.** `B`'s delayed command names `expect = (e, h_A)`
   while the head is `(e + 1, h_C)`; Store returns `Superseded((e + 1, h_C))` before any write.
2. **Deposed local retry duel.** `A`'s local supervisor retries with `expect = (e, h_A)` after
   `B` won `(e + 1, h_B)`; Store refuses `Superseded` and `Deposed` parks `A`'s assignment.
3. **Attached-stream janitor claiming back a moved Place.** `A`'s janitor ask carries the
   StoreId and the pair `(e, h_A)` its service observed, and opens with
   `Continue(expect = (e, h_A))`; if `H` already installed `(e + 1, h_B)`, the janitor fails
   `Superseded` before writing any `Held` row or head replace. An ask whose cause is already
   gone ends `JanitorCleaned` with zero writes.
4. **Lost `H` reply before `CLAIM`.** `H` already wrote `ORIGIN{permit_attempt = X.attempt}`.
   One `settle_permit` settles `X` as `HeadWon(e + 1)` even if `B` crashed before `CLAIM` or
   made local `Continue` claims afterwards.

## FLT-3: Control Place (`fleet@1`) and controller takeover

`fleet@1` is an ordinary standalone durable `Place` on `objects@1`.

- **Persisted state (durable in `fleet@1`):** fleet identity, node enrollments, incarnations
  and revocations, root-signed deploy keysets, operator-signed deploy approvals, desired
  workloads and registered cold Places, rollout wave state, drain intent and disruption limits,
  and one write-ahead permit row per outstanding Place move.
  A host mints its own incarnation: each open of its catalog commits one more (RUNNER-SPEC
  §14.12.1), and fleet@1 records the value the host reports under the `NodeId` it enrolled.
  There is exactly one incarnation per node. A host that lost its disk opens at incarnation 1
  with no enrollment, and fleet@1 enrolls it under a fresh `NodeId`, so `(node, incarnation)`
  never repeats.
- **Derived state (volatile RAM, zero bucket commits):** heartbeats, capacity scores, route hints
  and load rankings (`fleet@1` never holds Archetype C compute placement; see [compute.md](compute.md)).
- **Controller takeover rule:** Only members of the restricted standby roster hold write
  credentials for `fleet@1`'s prefix. A standby candidate observes the current `(epoch, holder)`
  and bucket-committed progress (any control transaction counts as progress; an idle controller
  commits an explicit beat). Only after observing unchanged progress for a full healthy window
  (plus jitter) does a candidate attempt takeover, conditioning its head CAS on **both** the
  probed `(epoch, holder)` **and** unchanged progress. A resumed beat resets suspicion and
  prevents ordinary catch-up from deposing a recovered leader.

## FLT-4: Scoped takeover barrier, membership, routing and drain

- **Scoped takeover barrier.** Immediately after controller takeover, unknown or unreported
  holders start as `Unknown`, never as orphaned. A new controller recovers pending permit rows
  first, matches fresh head witnesses to committed permits, and probes affected incumbents
  directly (and through two independent healthy witnesses where available) for a full
  healthy-observer window (default `6 s` with `1 s` jittered beats) measured inside the new
  controller's own incarnation. The barrier is scoped **per affected Place or incumbent node**:
  one slow or silent host never stalls unrelated Places or healthy hosts.
- **Membership and correlated-disruption breaker.** Star-shaped volatile authenticated
  heartbeats (`1 Hz` jittered) with controller self-health inflation (Lifeguard principle) and
  two cross-domain witness probes before suspicion. If a majority of a zone turns suspect or
  the controller's own observer loop lags, a day-1 correlated-disruption circuit breaker freezes
  discretionary moves and thaws only after two consecutive healthy windows.
- **Destination reservation and phased drain.** Destinations independently enforce
  `(incarnation, generation, capacity)` reservations and cordon checks before consuming a
  permit. On a host, reservations hold place slots in the host's own capacity ledger, one row
  per Place replaced by a higher generation, and end at the Place's assignment or at the end
  of the incarnation that made them (RUNNER-SPEC §14.12.3). Drain proceeds in phases (close admission, warm/transfer, collect settled receipts)
  against declared disruption budgets.
- **Routing.** Durable intent travels over scoped `CoverSet` subscriptions; volatile placement
  snapshots and deltas carry `(issuer_epoch, seq)` plus per-key monotone range generations
  (`merge = max(parent_a, parent_b) + 1`), bounded redirect hops and full snapshot repair on
  cursor gaps.

## FLT-5: Dedicated `Directory[K]` Places (Archetype B)

`Directory[K]` (`core/programs/fleet/directory.hirsel`, `core/programs/fleet/directory_place.hirsel`)
runs in dedicated, ordinary durable `Place`s partitioned by keyspace — never inside `fleet@1` —
so controller takeovers, disruption freezes and Fleet deploys never stall ephemeral room, relay
or cache failovers.

### Partitioning and durable Place schema

Each `Directory[K]` Place owns one closed keyspace interval `[min_key, max_key]` compared over
the canonical order-preserving `std.codec.Key[K]` byte encoding. A key outside the partition's
bounds is refused with `OutOfPartition`.

Each partition stores two durable collections inside its own `store.Declare` schema (never
shared with `fleet@1`'s tables):

- **`<name>.meta` (`store.Cell[PartitionMeta]`):** durable partition identity (`partition_id`),
  keyspace bounds (`min_key_bytes`, `max_key_bytes`), fleet drift bound `drift_ppm` (`d`),
  durable lease-duration high-water mark `max_lease_ns` (`L_max`), and monotone directory Place
  epoch `dir_epoch`.
- **`<name>.leases` (`store.Table[K, LeaseRow]`):** per-key term CAS state: `(holder_node,
  holder_incarnation, term, lease_ns, dir_epoch, last_dir_boot_ns, lease_end_boot_ns,
  last_holder_boot_ns, seq, payload)`.

```hirsel
pub type DirectoryConfig: codec + key + send {
    partition_id: u64,
    drift_ppm: u64,
    default_lease_ns: u64,
    max_lease_cap_ns: u64
}

pub type LeaseGrant: codec + key + send {
    term: u64,
    lease_ns: u64,
    wait_ns: u64,
    dir_boot_ns: u64,
    dir_epoch: u64,
    seq: u64
}

pub type HolderLease: codec + key + send {
    holder_node: u64,
    holder_incarnation: u64,
    term: u64,
    send_boot_ns: u64,
    lease_ns: u64,
    expires_boot_ns: u64
}

pub type DirError: codec + key + send = Stale(u64) | Held(u64) | Quarantined(u64) | DriftExceeded(u64, u64) | OutOfPartition | InvalidConfig | Unknown | StoreErr
```

### Normative rules `D1`–`D6`

- **D1 (Durable renewal barrier).** The directory releases a `claim`, `renew` or `release`
  reply only after the durable commit (`store.Tx` / `store.durable` or `runner.transact`) that
  started after the request arrived is observed `Applied` (amortized across a batching
  interval).
- **D2 (Suspend-inclusive elapsed clock).** Both the directory and every lease holder measure
  lease windows strictly with `std.clock.boot_ns(clock)` (`CLOCK_BOOTTIME` /
  `mach_continuous_time`). A monotonic clock that freezes across OS suspend or VM pause
  (`host.mono_ns`) is forbidden for lease deadlines: across a pause of duration `>= L`, a
  paused holder's `boot_ns` advances past its deadline while `mono_ns` stays frozen.
- **D3 (Drift-bounded lease wait `W`).** For lease duration `L` (nanoseconds) and fleet clock
  drift bound `d = drift_ppm / 10^6` (`0 <= drift_ppm <= MAX_DRIFT_PPM < 10^6`), the directory
  computes the exact integer ceiling:

  `W(L, d) = ceil(L * (1 + d) / (1 - d)) = L + floor(L / (PPM - d)) * 2d + ceil((L mod (PPM - d)) * 2d / (PPM - d))`

  with zero floating-point rounding and no `u64` overflow. A holder starting a request at
  `send_boot_ns` stops believing it holds the lease at `send_boot_ns + L` (at most `L / (1 - d)`
  true time), while the directory waits at least `W(L, d)` on its own `boot_ns` (at least
  `W(L, d) / (1 + d) >= L / (1 - d)` true time) before granting any conflicting new term.
- **D4 (Takeover quarantine wait `W`).** When a replacement host opens a `Directory[K]` Place
  (`takeover_partition_tx`), it increments durable `dir_epoch`, captures its own local
  `takeover_boot_ns = std.clock.boot_ns(clock)`, and establishes a volatile session quarantine
  `quarantine_until_boot_ns = takeover_boot_ns + W(L_max, d)` from the durably stored
  `L_max = meta.max_lease_ns` (for genesis with `L_max == 0`, `W = 0`). During quarantine:
  - Same-term `renew` by an existing durably-recorded incumbent resumes immediately (updating
    durable `seq`, `lease_ns`, `dir_epoch`, `last_dir_boot_ns` and `lease_end_boot_ns`).
  - Any new-term `claim` is refused with `Quarantined(quarantine_until_boot_ns)` until
    `now_boot_ns >= quarantine_until_boot_ns`.
- **D5 (Drift bound enforcement and refusal).** A partition configuration with
  `drift_ppm > MAX_DRIFT_PPM` (or `drift_ppm >= 1_000_000`) or `default_lease_ns == 0` is
  refused with `InvalidConfig` / `DriftExceeded`. Every `claim` and `renew` carries the
  caller's declared oscillator drift bound `reported_drift_ppm` and current `holder_boot_ns`
  (plus bounded `max_rtt_ns`). The directory refuses with `DriftExceeded(observed_ppm, limit_ppm)`
  whenever:
  1. `reported_drift_ppm > config.drift_ppm`,
  2. `holder_boot_ns < row.last_holder_boot_ns` (regressed clock), or
  3. across a positive directory interval `dir_delta = now_boot_ns - row.last_dir_boot_ns`
     exceeding `max_rtt_ns`, the holder's elapsed `holder_delta = holder_boot_ns - row.last_holder_boot_ns`
     falls outside the drift envelope checked with exact 128-bit integer products:
     `holder_delta * (PPM + d) < (dir_delta - max_rtt_ns) * (PPM - d)` (slow clock) or
     `holder_delta * (PPM - d) > (dir_delta + max_rtt_ns) * (PPM + d)` (fast clock).
- **D6 (Term CAS and holder exclusivity).** `claim(key, holder, expect_term, ...)` checks
  partition bounds, drift, takeover quarantine, `expect_term == current_term` (`Stale(cur)` on
  mismatch), and active lease expiration `now_boot_ns >= row.lease_end_boot_ns` (`Held(until)`
  while active), then atomically increments `term = expect_term + 1`, updates
  `max_lease_ns = max(max_lease_ns, lease_ns)`, commits durably (D1) and returns `LeaseGrant`.
  Holders record `expires_boot_ns = send_boot_ns + grant.lease_ns` from request send time and
  verify `std.clock.boot_ns(holder_clock) < expires_boot_ns` before acting.

### Initial qualification profile (`sfu_cascade`)

Maximum home lease `L = 24 s`, renew every `8 s`, directory takeover budget `T = 8 s`
(`L:T = 3:1`), fleet drift bound `d = 200 ppm` (`W = 24,009,601,921 ns`), giving `<= 25.01 s`
home-loss failover and `<= 32.76 s` simultaneous home+directory failover under qualified bounds.
Senders recheck the suspend-inclusive deadline at socket handoff and latch closed on post-send
stall detection.

## FLT-6: Code trust, revocation clearance, transport and bucket boundaries

- **Operator-only attestation and canonical approval transcript (`v1`).** An offline root key
  pinned at join signs rotating deploy keysets (with monotone epoch and durable node
  anti-rollback marks). Operators sign deploy approvals covering `(fleet, group, generation,
  release, predecessor, recipe_root, assembly_target, grant_diff, held_fates)` serialized as the
  canonical `DeployApprovalTranscriptV1` transcript below, bound to the signing keyset entry's
  algorithm (`ed25519` or `p256`) and key identifier:

  ```hirsel unbuilt
  pub type DeploySigAlg: codec + key + send = AlgEd25519 | AlgP256

  pub type DeployApprovalTranscriptV1: codec + key + send {
      domain_label: Bytes,
      version: u8,
      alg: DeploySigAlg,
      keyset_epoch: u64,
      key_id: Bytes,
      fleet: Bytes,
      group: Bytes,
      generation: u64,
      release: Bytes,
      predecessor: Option[Bytes],
      recipe_root: Inline[u8, 32],
      assembly_target: Bytes,
      grant_diff: Bytes,
      held_fates: Bytes
  }
  ```

  Canonical wire bytes for signing and verification (little-endian, exact byte concatenation in
  field order, no padding, no trailing bytes):

  - `domain_label`: `u32` LE byte length (`31`) followed by exact ASCII bytes
    `"hirsel.fleet.deploy-approval.v1"`.
  - `version`: `u8` equal to `1`.
  - `alg`: `u8` wire tag (`1 = AlgEd25519`, `2 = AlgP256`; `0` and `>= 3` reserved/rejected);
    must equal the referenced keyset entry's declared algorithm.
  - `keyset_epoch`: `u64` LE (8 bytes).
  - `key_id`, `fleet`, `group`, `release`, `assembly_target`, `grant_diff`, `held_fates`: each
    encoded as a `u32` LE byte length followed by raw bytes; `grant_diff` and `held_fates` carry
    their canonical `std.codec` encodings ([SEM-10](semantics.md#sem-10-codecs)), with any
    map/set entries serialized in canonical sorted key order.
  - `generation`: `u64` LE (8 bytes).
  - `predecessor`: `u8` tag `0` for `None`, or `1` followed by `u32` LE byte length and raw
    bytes for `Some`.
  - `recipe_root`: 32 raw digest bytes (fixed width, no length prefix).

  Verifiers reject any unknown `version`, unknown or mismatched `alg` tag, wrong `domain_label`,
  non-canonical compound encoding, or trailing bytes. Signers and verifiers must agree
  byte-for-byte against a shared golden transcript vector. Ed25519 verification follows
  [core.md](core.md#signature-verification)'s cofactorless RFC 8032 acceptance rule (`S < L`,
  `[S]B − [k]A = R` compared against `R`'s canonical 32-byte encoding with raw `R` and `key` in
  `k`, rejecting cofactored-only and non-canonical-`R` signatures while accepting non-canonical
  `key` encodings and small- or mixed-order keys whenever the cofactorless equation holds).
  Hosts fetch missing objects by digest from the bucket and feed the launcher's existing
  `Admit -> Need -> AdmitData -> AdmitEnd` exchange (RUNNER-SPEC §14.12.4: `objects/<kind>/<hash hex>` under the
  code prefix; the launcher hashes every object it receives).
- **Revocation-ordered clearance and anti-rollback mark bootstrap.** Before starting any preview,
  prepare or serving child of a new assembly, a host executes `fleet.clear(...)` in one `fleet@1`
  `transact` ordered with `fleet.install_keyset(...)`. The clearance response is authenticated
  and bound to this host's enrolled `NodeId`, current enrollment/incarnation, and the
  `fleet.clear(...)` transaction nonce/version. The host verifies the returned keyset against the
  pinned root, **rejects any clearance whose keyset epoch is lower than its existing durable
  anti-rollback floor (or provisional join floor)**, durably persists the raised floor
  `max(existing_floor, keyset_epoch)` **before** accepting or activating any approval, and
  re-verifies the approval signer, `key_id`, and `alg` binding against that keyset.
  - **Fresh or disk-lost host bootstrap:** a host with **no durable anti-rollback mark** refuses
    all deploy approvals until it completes a fresh, authenticated, ordered `fleet.clear(...)`
    transaction against `fleet@1` (or an operator-authorized fresh re-enrollment carrying a
    current root-signed keyset) and durably persists that authoritative keyset epoch. An older
    saved join record supplies at most a **provisional lower bound** and never satisfies
    disk-loss bootstrap or authorizes approval acceptance on its own, preventing a wiped host
    from restoring an obsolete join epoch. An isolated host emits an honest
    `"staged, not cleared, nothing started"` receipt.
- **Transport.** Pinned [QUIC](quic.md) (`quic@1`) using SPKI netmap pins directly (no fleet
  CA). Every peer-authenticated request verifies the connection's authenticated pin via
  `quic.peer_pin` and binds it to the claimed peer **node identity** (`NodeId`) against the
  current enrolled roster/netmap pin set — never accepting an unverified peer-declared `NodeId`
  from application framing — with bounded live-session lifetime so revocations take effect on
  active connections.
- **Bucket credential boundaries (L1 minimum):** (1) bucket versioning / object lock preventing
  permanent deletion or version purge; (2) only pinned controller candidates hold write access
  to `fleet@1`'s prefix; (3) only the operator/publisher writes the code prefix. L2/L3 adds
  per-host revocable STS/IAM principals and per-security-domain streams when mutually
  distrustful tenants share a bucket.

## FLT-7: Proof-Ready Protocol Kernel contract (`K1`–`K13`)

Every Fleet, Store and Cluster protocol core follows one deterministic kernel shape so that
deterministic simulation, `std.model.conforms`, TLA+/TLC, Quint MBT and Veil/Lean proofs share
one canonical transition surface with zero compiler extensions:

```hirsel unbuilt
pub type KernelOutput {
    state: State,
    actions: List[Action],
}

pub fn step(state: State, ev: Event) -> KernelOutput
pub fn keys(ev: Event) -> List[Key]
```

The shell owns capabilities, storage, sockets, clocks and step budgets (`bound steps N { step(s, ev) }`).

| Rule | Normative requirement |
|---|---|
| **K1** | `step` takes no capability parameter and is not `waits`; no tasks, channels, finalizers, resource drops or host operations. |
| **K2** | No indirect calls inside `step` or `keys`; dispatch strictly on enums via `match`. |
| **K3** | Every collection in `State` enforces a declared capacity cap. The shell owns `bound steps` / `bound bytes` scopes; `step` contains no `bound` scope. Bound exhaustion is a crash handled by the shell and modeled in the abstract spec. |
| **K4** | `State`, `Event`, `Action`, `Key` and `Obs` are `codec` types ([SEM-10](semantics.md#sem-10-codecs)), giving one canonical byte encoding for tapes, NDJSON and ITF exports. |
| **K5** | Time enters only as typed events (`Tick` / `Timeout`); `step` never reads a clock. |
| **K6** | Ghost and refinement observations leave as `Action::Observe(Obs)`, never read back by the shell for control decisions. Wherever a real `Action` exists, the conformance `Boundary` maps the real action; `Observe` never stands in for an unverified sink effect. |
| **K7** | **K7a:** Safety at durable sinks is clock-free (enforced by `(epoch, holder)` / `term` CAS). **K7b:** Archetype B's lease exclusivity is a separate timed assume/guarantee proof at egress, kept outside the push-button safety kernel. |
| **K8** | Every kernel ships with an independently written abstract specification (`std.model`, plus TLA+, Quint and Veil for fencing cores) and a `Boundary` mapping kernel observations to abstract labels. |
| **K9** | The shell can capture `(Event, List[Action])` to a codec trace tape (always on in simulation; opt-in or sampled in production). |
| **K10** | Monotone advisory state (routing hints, heartbeats, load scores) lives in a separate CALM kernel whose merge is a join-semilattice and never feeds fenced safety decisions. |
| **K11** | **Atomic outbox (and volatile scope).** The shell persists the new `State` and emitted outbox actions atomically before releasing any send/grant action; actions may be delivered more than once and are replayed by id. **Volatile scope (K11-V):** a volatile ledger that persists nothing and releases directly is admissible only where every released execution either dies with its ledger or is accounted for before the slot is reused ([compute.md](compute.md)); durable control changes, limit-right transfers, receiver-committed effects and promised durable completions keep the durable rule at their owning authority. |
| **K12** | **Bounded read set.** Pure `keys(ev)` selects the bounded set of entity and budget keys read at one consistent version and committed conditionally; a fleet of a million Places never enters one `step` call. |
| **K13** | **Total outcomes.** Every shell or sink outcome is fed back as a typed `Event`: `Won`, `Lost`, `Superseded`, `Held`, `Refused`, `Undecided`, `Timeout`, `Duplicate`, `WrongIncarnation`, `LateUnknownId`, `Crash`, `Restart` and `BoundExhausted`. Exhaustion before commit releases no actions. |

## FLT-8: Verification tiers (`T0`–`T3`) and `SM1` traces

Proof checks run as explicit targets or nightly campaigns and never gate ordinary pre-push
commits.

- **T0 (Deterministic simulation & controller laws).** `hirsel fleet sim`
  drives the production per-Place move kernel, advisory lattice and partitioned `Directory[K]`
  Places across simulated hosts (qualified at 50 and 1,000 hosts) with an independent concurrent
  register, message hold/release, node crash/restart, clock pause, controller kill mid-move,
  zone isolation, Sieve stale-controller laws, Acto desired-state edit laws and SM1 trace
  verification.
- **T1 (Trace conformance & `SM1`).** Traces from simulation are checked in-process with
  `std.model.conforms` against the independent concurrent register model, and exported in
  `SM1` canonical format (lossless integers `> 2^53`) as NDJSON for TLA+/TLC trace validation
  and ITF JSON for Quint model-based testing.
- **T2 (Unbounded-N inductive proof).** The abstract fencing protocol is proved inductive for
  arbitrary node counts and epochs in Veil (Lean 4) — or TLAPS if Veil cannot express the
  invariant — and cross-checked against bounded TLC before enabling production controller
  takeovers.
- **T3 (Code extraction gate).** `hirsel lean probe` runs directly on the production kernel
  reducer (`step` and `keys`). Only a clean probe opens CoreIR-to-Lean forward-simulation
  extraction; any unmodelled instruction kinds (such as `Alloc`, `Free`, `Grow` or `Release`)
  are recorded as explicit T3 gaps without blocking T0–T2.
