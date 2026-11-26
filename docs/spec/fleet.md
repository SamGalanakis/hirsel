# Fleet

*Specified 2026-11-26 (converged Hirsel Fleet blueprint, DECISIONS.md). Rules FLT-1 to FLT-8,
kernel rules K1 to K13, verification tiers T0 to T3. Phase 1 builds the move-authorization
kernel, independent register model, simulation laws and formal proofs; orchestration, Store
`ClaimAuth`/`ORIGIN`, Runner `Deposed`, launcher envelope and platform seams remain unbuilt
and are marked with `unbuilt` fences.*

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
| **C · Elastic Remote `Compute`** | Ray/FLAME-style tasks, shuffles, batch and AI jobs (`compute@1`) | None (pure retries via [SEM-8](semantics.md#sem-8-determinism); effectful sinks via receiver-checked keyed idempotency) | **Never** | Dispatch `(release_id, entry_name, codec_args, bounds)` to a pre-staged ready host (never ship functions, [PROG-9](programs.md#prog-9-code-descriptors)) |

## FLT-2: Move protocol (`ClaimAuth`, `ORIGIN`, `Superseded`, settlement, `Deposed`)

### Universal `ClaimAuth`

Every non-genesis `objects@1` writer open — moves, local restarts, installs, serving
candidates, attached-stream janitors and explicit repairs — carries a `ClaimAuth` precondition:

```hirsel unbuilt
pub type HeadPair: codec + key {
    epoch: u64,
    holder: Inline[u8, 16],
}

pub type ClaimClass: codec + key = Permit | Continue | Adopt

pub type ClaimAuth: codec + key {
    expect: Option[HeadPair],
    attempt: Inline[u8, 16],
    class: ClaimClass,
}

pub type ClaimRefusal: codec + key = Superseded(HeadPair) | Lost | Corrupt
```

- **Checked before any write.** The store checks `ClaimAuth.expect` against the head's
  `(epoch, holder)` at first GET before issuing any write, including before writing any
  attached-stream `Held` row (§12.11). On mismatch it returns `Superseded(actual)` and performs
  no write.
- **Genesis and Adopt.** Conditional creation uses a distinct authorized `Genesis(Absent)`
  operation. `expect: None` is permitted only with `class = Adopt` from an explicit local
  operator action on a host with no recorded pair, never from a restart, wake, retry, janitor
  or remote message, and never on a fleet-owned namespace.
- **Fresh random `holder` per physical open.** Every physical open draws a fresh, private
  random 16-byte `holder` reused only for retries inside that open. `holder` is never reused as
  `attempt` across duplicate command deliveries: `history_write_head` resolves `Mismatch` by
  byte equality, so identical proposals from two physical opens would return `HistoryApplied`
  to both and create dual writers.

### The `ORIGIN` head frame

The claim's head replace (`H`) writes a 49-byte `ORIGIN` frame (`MUST = 1`) atomically with the
new `(epoch, holder)` pair:

```hirsel unbuilt
pub type OriginFrame: codec + key {
    epoch: u64,
    attempt: Inline[u8, 16],
    class: ClaimClass,
    permit_epoch: u64,
    permit_attempt: Inline[u8, 16],
}

pub type HeadView: codec + key {
    pair: HeadPair,
    origin: Option[OriginFrame],
}
```

Wire payload layout (49 bytes, little-endian, `MUST = 1`):

- `epoch: u64` (8 bytes) — the epoch installed by this claim; equals `HEAD.epoch`.
- `attempt: Inline[u8, 16]` (16 bytes) — this claim's attempt identifier.
- `class: u8` (1 byte) — `0 = Permit` (Fleet move), `1 = Continue` (same host: restart,
  install candidate, janitor), `2 = Adopt` (explicit operator first open).
- `permit_epoch: u64` (8 bytes) — epoch installed by the latest `Permit` claim in this head's
  lineage; `0` if none.
- `permit_attempt: Inline[u8, 16]` (16 bytes) — attempt identifier of that `Permit` claim; all
  zeros if none.

Rules **O1–O4**:

- **O1.** `ORIGIN` is written as part of the head replace `H` that installs `(e + 1, holder)`.
  It exists if and only if the cut applied, closing the crash window where `H` succeeds and a
  subsequent `CLAIM` entry or reply is lost.
- **O2.** Every ordinary advance, `CLAIM`, cut, settle or direct-service head write by that
  holder copies `ORIGIN` byte-for-byte.
- **O3.** A `Continue` or `Adopt` claim copies `permit_epoch` and `permit_attempt` from the
  head it replaces. A `Permit` claim sets both to its own `epoch` and `attempt`.
- **O4.** Any head carrying `ORIGIN` satisfies `ORIGIN.epoch == HEAD.epoch`, else
  `Corrupt(Malformed)`. A head without `ORIGIN` reads an empty permit slot and counts as
  `permit_attempt != X` during settlement. Because `MUST = 1`, the first claim by an
  `ORIGIN`-writing binary is one-way for that Place: older binaries refuse the head with
  `Unsupported(kind)`.

### Read-only settlement and Rule F1

Fleet persists a write-ahead permit row `X { expect: (e, h_A), target: B }` before releasing
any move command. A single read-only head GET (`HeadView`) settles `X` without opening a writer
or reading a tail:

```hirsel unbuilt
pub type Settlement: codec + key = Open | Won(u64) | Dead(HeadPair)
```

| Head read (`HeadView`) | Settlement | Controller action |
|---|---|---|
| `pair == (e, h_A)` | **`Open`** | `X`'s CAS may still apply. Reading the old pair never cancels `X`. To force progress against a dead target, authorize one bounded recovery contender sharing the same `expect`. |
| `origin.permit_attempt == X` at `HEAD.epoch >= e + 1` | **`Won(permit_epoch)`** | Close `X` as won. `HEAD.epoch - permit_epoch` counts `B`'s subsequent local `Continue` claims. Any later move names the head's current pair. |
| `HEAD.epoch > e` and `origin.permit_attempt != X` | **`Dead(pair)`** | Epoch `e` was consumed by another claim (`Continue` on `A` or a recovery contender); `X` can never apply. Close `X` as superseded and never resend it. |

- **Rule F1 (single predecessor per open set).** The control transaction that authorizes a
  `Permit` on Place `P` requires that every open attempt on `P` shares the new attempt's
  `expect`. While `X` is open, only `X` and a recovery contender with the same `expect` can
  install a new `Permit`. Fleet settles from the read-only `HeadView` before authorizing any
  move from the successor pair.

### Deposed end cause and the three stale-open races

Runner adds one end cause, `Deposed(pair: HeadPair)`. When a child is fenced, the host parks
the assignment and stops local restart retries. Even if the end report is lost, a retry still
names the old `expect` pair and receives `Superseded(actual)`.

Together, `ClaimAuth`, `ORIGIN` and `Deposed` close all three stale-open races and the lost-`H`
window:

1. **Delayed `A -> B` move after `C` wins.** `B`'s delayed command names `expect = (e, h_A)`
   while the head is `(e + 1, h_C)`; Store returns `Superseded((e + 1, h_C))` before any write.
2. **Deposed local retry duel.** `A`'s local supervisor retries with `expect = (e, h_A)` after
   `B` won `(e + 1, h_B)`; Store refuses `Superseded` and `Deposed` parks `A`'s assignment.
3. **Attached-stream janitor claiming back a moved Place.** `A`'s in-flight janitor passes
   `expect = (e, h_A)` with `Continue`; if `H` already installed `(e + 1, h_B)`, the janitor
   fails `Superseded` before writing any `Held` row or head replace.
4. **Lost `H` reply before `CLAIM`.** `H` already wrote `ORIGIN{permit_attempt = X}`. One
   read-only `HeadView` read settles `X` as `Won(e + 1)` even if `B` crashed before `CLAIM` or
   performed local `Continue` restarts afterwards.

## FLT-3: Control Place (`fleet@1`) and controller takeover

`fleet@1` is an ordinary standalone durable `Place` on `objects@1`.

- **Persisted state (durable in `fleet@1`):** fleet identity, node enrollments, incarnations
  and revocations, root-signed deploy keysets, operator-signed deploy approvals, desired
  workloads and registered cold Places, rollout wave state, drain intent and disruption limits,
  and one write-ahead permit row per outstanding Place move.
- **Derived state (volatile RAM, zero bucket commits):** heartbeats, capacity scores, ready
  compute slots, route hints and load rankings.
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
  permit. Drain proceeds in phases (close admission, warm/transfer, collect settled receipts)
  against declared disruption budgets.
- **Routing.** Durable intent travels over scoped `CoverSet` subscriptions; volatile placement
  snapshots and deltas carry `(issuer_epoch, seq)` plus per-key monotone range generations
  (`merge = max(parent_a, parent_b) + 1`), bounded redirect hops and full snapshot repair on
  cursor gaps.

## FLT-5: Dedicated `Directory[K]` Places (Archetype B)

`Directory[K]` runs in dedicated ordinary durable Places partitioned by keyspace — never inside
`fleet@1` — so controller takeovers and Fleet deploys never stall ephemeral room or cache
failovers.

- **Rule D1 (durable renewal barrier).** The directory releases a `renew` reply only after a
  durable commit of its Place that started after the request arrived is observed `Applied`
  (amortized across a batching interval).
- **Quarantine wait on takeover.** After a directory takeover, same-term renewals of an
  existing incumbent resume immediately. Any conflicting new-term grant waits
  `W = ceil(L_max * (1 + d) / (1 - d))` on the replacement directory's own suspend-inclusive
  elapsed clock (`CLOCK_BOOTTIME` / `mach_continuous_time`), where `L_max` is the largest still-
  outstanding lease policy stored durably and `d` is the fleet drift bound.
- **Initial qualification profile (`sfu_cascade`).** Maximum home lease `L = 24 s`, renew every
  `8 s`, directory takeover budget `T = 8 s` (`L:T = 3:1`), giving `<= 25.01 s` home-loss
  failover and `<= 32.76 s` simultaneous home+directory failover under qualified bounds. Senders
  recheck the suspend-inclusive deadline at socket handoff and latch closed on post-send stall
  detection.

## FLT-6: Code trust, revocation clearance, transport and bucket boundaries

- **Operator-only attestation.** An offline root key pinned at join signs rotating deploy
  keysets (with monotone epoch and durable node anti-rollback marks). Operators sign deploy
  approvals covering `(fleet, group, generation, release, predecessor, recipe_root,
  assembly_target, grant_diff, held_fates)` verified with `ed25519` / `p256`. Hosts fetch
  missing objects by digest from the bucket and feed the launcher's existing
  `Admit -> Need -> AdmitData -> AdmitEnd` exchange.
- **Revocation-ordered clearance.** Before starting any preview, prepare or serving child of a
  new assembly, a host executes `fleet.clear(...)` in one `fleet@1` `transact` ordered with
  `fleet.install_keyset(...)`. The host verifies the returned keyset against the pinned root,
  raises its anti-rollback mark and re-verifies the approval signer. An isolated host emits an
  honest `"staged, not cleared, nothing started"` receipt.
- **Transport.** Pinned [QUIC](quic.md) (`quic@1`) using SPKI netmap pins directly (no fleet
  CA), checking the peer pin per request with bounded live-session lifetime so revocations
  take effect on active connections.
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
| **K11** | **Atomic outbox.** The shell persists the new `State` and emitted outbox actions atomically before releasing any send/grant action; actions may be delivered more than once and are replayed by id. |
| **K12** | **Bounded read set.** Pure `keys(ev)` selects the bounded set of entity and budget keys read at one consistent version and committed conditionally; a fleet of a million Places never enters one `step` call. |
| **K13** | **Total outcomes.** Every shell or sink outcome is fed back as a typed `Event`: `Won`, `Lost`, `Superseded`, `Held`, `Refused`, `Undecided`, `Timeout`, `Duplicate`, `WrongIncarnation`, `LateUnknownId`, `Crash`, `Restart` and `BoundExhausted`. Exhaustion before commit releases no actions. |

## FLT-8: Verification tiers (`T0`–`T3`) and `SM1` traces

Proof checks run as explicit targets or nightly campaigns and never gate ordinary pre-push
commits.

- **T0 (Deterministic simulation & controller laws).** `hirsel fleet sim` <!-- doc-ref: planned -->
  drives the production per-Place move kernel and shell across simulated hosts with an
  independent concurrent register, message hold/release, node crash/restart, clock pause,
  Sieve stale-controller laws, Acto desired-state edit laws and a client-history checker.
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
