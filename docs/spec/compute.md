# Compute

Archetype C compute placement lives exclusively in this
specification and never inside `fleet@1`
([FLT-1](fleet.md#flt-1-three-workload-archetypes),
[FLT-3](fleet.md#flt-3-control-place-fleet1-and-controller-takeover)).

`compute@1` defines worker, credit desk and dispatcher reducers for Archetype C
elastic remote workloads. Workers alone admit work against their capacity vectors.
`fleet@1` neither tracks compute slots nor sits on the create path.

A **principal** is the owner of grants on whose behalf compute runs. Every create names exactly
one principal; the principal's grants bound what the execution may do, and its hard
concurrency limit (COMP-2) bounds how many of its executions may be live at once.

## COMP-1: Separation from Archetypes A and B

Compute shares the single binary and enrolled hosts with Archetypes A (durable `Place`) and B
(ephemeral `Directory[K]` units), while isolating both their safety authority and their runtime
liveness:

- **Credential and authority separation.** Compute roles hold **no `fleet@1` write credential**
  and **no Place claim authority** (`ClaimAuth`, `ORIGIN`, or `Directory[K]` term grants). A
  compromised or faulty compute role cannot move a Place, authorize a writer, reassign a room, or
  act as a takeover witness. Compute modules must not import the Archetype A move kernel's
  `OutboxCmd` or `Key` (`core/programs/fleet/types.hirsel`), enforced as a build-time boundary
  check.
- **Disjoint host budgets.** Each enrolled host carries two strictly disjoint resource budgets set
  in its enrollment record: `place_reserve` (for Archetypes A and B) and `compute_pool` (for
  Archetype C). Borrowing in either direction is forbidden; resizing either budget is an explicit
  operator drain-then-resize action.
- **Separate QUIC endpoints and a bounded control path.** Control traffic (`fleet@1` heartbeats,
  move commands, clearance, and `Directory[K]` lease renewals) and compute data-plane traffic run
  on **separate [QUIC](quic.md) endpoints** with separate drive tasks and reserved CPU and
  transport turns for control traffic. Every queue the two share below the endpoints (socket send
  buffers, the host's egress queue discipline) serves control traffic at strict priority, so a
  control packet waits behind at most one compute packet already in transmission per shared
  queue on the sending host. The control-path service delay `d_ctl` is therefore bounded by the
  number of shared queues, their link rates, and the largest compute packet, not by compute load.
  Control deadlines (heartbeat suspicion, lease renewal) carry at least `d_ctl` of slack, so a
  saturating compute burst never delays heartbeats past their deadline, causes false suspicion,
  or trips [FLT-4](fleet.md#flt-4-scoped-takeover-barrier-membership-routing-and-drain)'s
  correlated-disruption breaker (`ControlLiveness`, COMP-9).

## COMP-2: Worker-only admission and the capacity ledger

The **worker** is the sole admission authority for compute execution, deciding on its full
physical resource vector with **zero physical overcommit**. Only the worker's admission ledger
can admit a slot or issue a physical host capacity refusal (`WorkerCapacityRefusal`; placement
routing misses are accounted distinctly in COMP-3 and COMP-4).

### Physical ledger

The worker keeps one **residual vector** `residual[d]` per physical dimension `d` of
`compute_pool`: memory bytes, virtual address space, map count, file descriptors,
connection-table entries, CPU, disk IOPS, concurrently mounted async stacks, and start
concurrency. Each staged compute class `k` declares a demand vector `need[k][d]`. Warm stock for
`k` already holds a part `warm[k][d]` of that vector; a slot's **incremental vector** is
`need[k][d] - warm[k][d]` for a slot drawn from warm stock and `need[k][d]` otherwise. Every
class on the host draws on the same residual vector, so admissible capacity is derived, never
stored: `capacity[k]` is the largest `n_warm + n_fresh` with `n_warm <= warm_slots[k]` and, for
every dimension `d`,

```text
n_warm * (need[k][d] - warm[k][d]) + n_fresh * need[k][d] <= residual[d]
```

so warm and fresh slots draw on one residual and are never counted twice.

- **Warm stock.** **Warm** debits `warm[k]` from `residual` and adds one warm slot of `k`;
  **Evict** removes an idle warm slot and returns `warm[k]`. A Mint from a warm slot consumes
  that warm slot. A slot's occupancy is its whole `need[k]` (the warm part plus its hold), and
  its retirement returns all of `need[k]` to `residual`; the slot is not warm again until a new
  **Warm**.

- **One hold per slot, reserved at Mint.** **Mint** (COMP-3) debits the slot's incremental vector
  from `residual` atomically across all dimensions and records it as that slot's **hold**. A
  class is offered only while its incremental vector fits; two classes can never both be offered
  the last unit of a shared dimension. **Redeem** converts the hold into occupancy without a
  second physical check and without a second debit. **Withdraw** returns exactly that hold.
  Downstream exhaustion paths (unit stack mapping, Wasm `HostMemory`, or launcher descriptor
  exhaustion) are unreachable from compute.
- **Per-create checks at Redeem.** Redeem re-checks everything that can change after Mint: caller
  authentication, principal authorization and grants, the principal's free right (below), host
  cordon state, [FLT-6](fleet.md#flt-6-code-trust-revocation-clearance-transport-and-bucket-boundaries)
  release clearance and anti-rollback marks, and grant subset. A Redeem that fails any of these
  checks Withdraws the credit (returning its hold) and answers with the matching definitive
  refusal; the popped credit never stays offered.
- **Lifetime boundaries.** Warm stock (until **Evict**), holds, in-flight starts, inert checkouts, active
  occupants, and retiring children remain charged against `residual` until the matching
  retirement transition of COMP-3 returns them (`NoOverAdmit`). Start completion returns only
  the start-concurrency term, never occupancy.
- **Shedding and control protection.** Under overload, a worker stops minting and sheds excess
  create traffic before expensive decode or allocation while protecting retirement, revocation,
  and control work. Limit refusals are reported distinctly from host saturation
  (`Refused(Limit)` vs. `Refused(Capacity)`, COMP-4).

### Hard per-principal concurrency limits

Each principal `p` has a concurrency limit: the number of its executions that may be live at
once. Limits are **hard**: zero overshoot, including during partitions, restarts, rebalancing,
and desk failover. They are enforced by **escrowed rights**, which keep two ledgers apart:

| Ledger | Owner | Fields per principal `p` | Durability |
|---|---|---|---|
| Rights assignment | worker `w` | `assigned[p]` and the escrow-table epoch `installed` it came from | durable at the worker (FLT-7 K11), written only when an escrow table is installed, never on the create path |
| Spending | worker `w`, per process `run` | `free[p]`, `charged[p]` (a set of `(slot, generation)`), `frozen[p]` | volatile (K11-V), rebuilt only by COMP-5 |

- **Local balance.** At every stable state of a run,
  `free[p] + count(charged[p]) + frozen[p] = assigned[p]` (`EscrowBalance`). The desk and the escrow
  table see only `assigned`; `free` is never reported as, or installed over, an assignment.
- **Debit with the slot.** Redeem moves one right from `free[p]` to `charged[p]` under the new
  slot's `(slot, generation)` in the same atomic step that converts the hold. With
  `free[p] = 0` the worker refuses before running (`WorkerLimitRefusal`, a definitive first-send
  refusal). Offers (COMP-3) carry the principals with `free[p] > 0`, and the desk grants an offer
  to a create for `p` only from such an offer.
- **Return on matching retirement.** A right leaves `charged[p]` only through the retirement
  transition of COMP-3 whose `(host, run, slot, generation, principal)` matches its charge, and
  goes to `frozen[p]` while a decrease is pending (below) and to `free[p]` otherwise. Expiry,
  desk restart, settlement, or loss of contact never returns a right. A duplicate or late
  retirement for a reused slot matches nothing and changes nothing.

### Escrow tables and transfers

A principal's allocation in a cell (COMP-8) is split into per-worker assignments by the cell's
**escrow table**: an epoch-versioned list of `assigned[p][w]`, signed by the cell's **rights
authority**. The rights authority is the external operator or program tool that holds the cell's
escrow signing key, alongside the federation manifest's signer (COMP-8). It needs no `fleet@1`
write credential (COMP-1). It keeps its chain durably (K11): it signs **exactly one successor
per table**, and records that table before releasing it. Every table therefore has one
predecessor, and two plans built from the same predecessor can never both be signed.

- **Decrease, then increase.** Each table only lowers assignments or only raises them. A table
  that raises any assignment is signed only after every worker that an earlier table lowered has
  acknowledged that lowering. Moving `k` rights of `p` from worker `A` to worker `B` is
  therefore one **transfer** `t = (transfer_id, p, k, A, B, lower_epoch)` carried by two tables:
  the lowering table at `lower_epoch`, then a raising table that names `t`.
- **Donor surrender.** When `A` receives a table that lowers `assigned[p]` by `k`, it moves free
  rights into `frozen[p]` until `frozen[p] = k`; rights that retire while the decrease is pending
  go to `frozen[p]` first. Only free rights move: a charged right is never surrendered.
  When `frozen[p] = k`, `A` durably installs the table (`assigned[p] -= k`, `frozen[p] = 0`,
  `installed = lower_epoch`) and only then emits `DecreaseAck(transfer_id, A, lower_epoch)`. A
  partition or a busy worker can hold that acknowledgement indefinitely; the transfer then waits.
  The rights authority redelivers the latest table to each worker until that worker acknowledges
  it, so a donor that restarts with a lowering pending (its durable `installed` still older)
  receives it again and surrenders after COMP-5 rebuilds its spending ledger.
- **One acknowledgement, one increase.** The rights authority consumes each `DecreaseAck` once:
  the single raising table that names `transfer_id` is the only table it backs. A replayed
  acknowledgement (FLT-7 K11, replay by action ID) returns that same signed table and backs
  nothing new.
- **In transit until applied.** Between the raising table's signature and `B`'s durable install,
  the `k` rights are in transit: counted in the table, spendable by no worker. `B` installs a
  table only if its epoch is above `installed`; installing it again is a no-op, so `B` applies
  `t` exactly once. A recipient that crashes before installing installs the same table on return.
  Rights in transit are never restored to `A` on a timeout or by an operator log entry; returning
  them is a new transfer with `B` as donor.
- **Conservation.** For every principal `p` and cell `c`, the sum over workers of `assigned[p]`
  at each worker's own installed epoch never exceeds the cell's effective allocation
  `allocation[p][c]` (COMP-8) (`EscrowConservation`),
  although workers run at different epochs.
- **Stranded during a partition.** Rights assigned to an unreachable worker stay with it until it
  returns. The rights authority reassigns them without that worker's acknowledgement only by
  **retiring the host**: a signed roster change removing the host, together with an operator
  attestation that the host process and every compute child on the host are dead and cannot
  start again holding the rights record (the host is powered off and its rights record erased
  before any host process runs again). A retired host identifier is never re-enrolled (COMP-6), and a worker whose
  current signed roster does not list its host never opens (COMP-5). The retirement acts as the
  donor's acknowledgement for every right that worker held, charged rights included, since none
  of its executions can still run.

`NoOverLimit`: for every principal `p` and cell `c`, the number of live admitted executions of
`p` in `c` never exceeds the effective `allocation[p][c]` (COMP-8). It follows from `EscrowBalance` and
`EscrowConservation`.

## COMP-3: Placement (credit desk)

Placement matches exact worker-funded capacity to demand at **one exclusive rendezvous per
shard** — the **credit desk** (equivalently, an exclusive ordinal pop at the shard relay, with
one active owner per worker-shard offer). All placement-specific roles, types, roster fields,
and safety invariants are encapsulated in this section.

All creates use the same credit desk. A worker reserves capacity before funding an
offer. The desk assigns neither slots from capacity reports nor overcommitted slots.

### Roles and exclusive credit protocol

Three reducers own the placement state:

- **Worker.** Owns its physical slots and mints exact, single-use credits only for capacity it
  has reserved (COMP-2 hold), within a bounded per-class window `W`. Credit ordinals are drawn
  from a per-`run` counter that only increases: every minted ordinal is fresh for its `run`, each
  credit `CreditId { worker_run, ordinal }` names one slot, an `Offer` may batch credits with
  consecutive ordinals, and each ordinal redeems at most once. A worker whose counter would wrap stops
  minting (fail closed). Every credit is scoped to the worker's live process `run` (COMP-6) and
  cannot redeem in any later `run`.
- **Credit desk (shard team).** A soft-state rendezvous on fixed roster-ordered **desk teams**
  named in the cell's signed roster, with each class hashed to one shard/team. In steady state,
  the highest-ranked member of the shard's team is the shard primary and owns the pop transition
  for every connected worker. Each desk member draws a fresh `desk_session` whenever it starts,
  and every worker stream it accepts is bound to that session by a handshake. The desk holds
  offers, deduplicated by `(worker, ordinal)` within its session, and per-dispatcher,
  per-principal demand counts (`Ask(class, principal, n)`) — never payloads — and pops each offer
  to at most one dispatcher (`GrantExclusive`). It drops any offer whose `desk_session` is not
  its current one. The shard uses self-clocked fair queueing with weights
  `allocation[p][cell]`. When an eligible waiting principal's head becomes
  ready, stamp it once with `max(F_prev[p], V) + 1 / allocation[p][cell]`, where `F_prev[p]` is
  the last served tag for that principal and `V` is the last tag served at the shard. Keep the
  tag while that head stays ready. Grant the head with the smallest stored tag, breaking ties
  by lowest principal ID. Set `F_prev[p]` and `V` to its tag, then stamp the next head if ready.
  A principal returning from idle starts at `V`. Tags are exact rationals (pairs of integers,
  FLT-7 K4), never floats. *(Team size, such
  as 3 hosts per team, is a non-normative deployment configuration parameter.)*
- **Dispatcher.** Runs in the trusted host process (never inside untrusted user code). Parks
  waiting creates within a bounded per-principal pending cap until a grant arrives or the caller's
  deadline expires; sends the create directly to the granted worker and returns the handle.
  Workers hold no queue. A create with no deadline receives an immediate `PlaceNoCredit` when the
  desk holds no matching-class offer, and `PlaceNoRight` when it holds matching-class offers but
  none carries a right for the create's principal.

```hirsel
pub type CreditId: codec + key + send {
    worker_run: u64,
    ordinal: u64
}

pub type SlotState: codec + key + send =
    SlotFree
    | SlotOffered(u64, u64, u64)
    | SlotStarting(u64, u64, bool)
    | SlotInert(u64, u64)
    | SlotActive(u64, u64)
    | SlotRetiring(u64, u64)

pub type PlacementReply: codec + key + send =
    PlaceUnknownCredit
    | PlaceCreditSpent
    | PlaceNoCredit
    | PlaceNoRight
```

`SlotOffered(ordinal, owner_epoch, desk_session)` names the slot's credit;
`SlotStarting(generation, principal, inert)` records whether the create is `effectful`; every
other non-free state is `(generation, principal)`.

### Worker slot machine and retirement

Worker transitions are serialized per worker and clock-free for safety; timers enter only as
typed `Timeout` events (FLT-7 K5). Each worker maintains a monotone per-shard
`owner_epoch: u64` and at most **one** active desk owner `(member, owner_epoch, desk_session)`
per shard at any instant. In the table, "hold" is the slot's COMP-2 hold, "start term" its
start-concurrency term, and "right" its COMP-2 charge `(slot, generation)` of principal `p`.

| Transition | From | Guard | Effect |
|---|---|---|---|
| **Mint** | `SlotFree` | in-flight offers and starts for the class `< W`; not cordoned, overloaded, or closed; incremental vector fits `residual` | debit the hold; draw a fresh ordinal `o`; `SlotOffered(o, owner_epoch, desk_session)`; emit `Offer(class, slot, CreditId { local_run, o }, owner_epoch, desk_session, principals with free > 0)` to the sole active desk owner |
| **Redeem** | `SlotOffered(o, _, _)` | caller authenticated; grant names `CreditId { local_run, o }`; every other COMP-2 per-create check passes, including `free[p] > 0` | advance the slot's `generation`; convert the hold to occupancy; move one right of `p` from `free` to `charged` under `(slot, generation)`; `SlotStarting(generation, p, inert)` with `inert` true for an `effectful` create; send the launch command to the launcher (`OneLiveCredit`, `SingleUse`) |
| **RedeemRefused** | `SlotOffered(o, _, _)` | caller authenticated; grant names `CreditId { local_run, o }`; another COMP-2 per-create check fails | as **Withdraw**; answer the matching definitive refusal (`WorkerLimitRefusal` when `free[p] = 0`) |
| **Withdraw** | `SlotOffered(o, e, s)` | stream reset of session `s`; `Abandon(slot, CreditId { local_run, o }, s)`; drain; overload; owner **Handoff**; or the offer's local hold `h`, timed from Mint, expires (the worker then mints a fresh credit at once if the slot is still fundable) | return the hold; `SlotFree`; ordinal `o` is permanently invalid (`WithdrawIsFinal`) |
| **Started** | `SlotStarting(g, p, inert)` | launcher reports the child of `(slot, g)` started | return the start term; `SlotInert(g, p)` if `inert`, else `SlotActive(g, p)` |
| **Activate** | `SlotInert(g, p)` | authenticated activation naming the live `(run, slot, g)` and passing COMP-6 checks | start user code holding grants; `SlotActive(g, p)` |
| **ActivateReplay** | `SlotActive(g, p)` | activation naming the live `(run, slot, g)` | no state change; answer `Activated` (idempotent; user code never starts twice) |
| **InertExpire** | `SlotInert(g, p)` | the inert checkout's bounded activation hold `a` expires | kill the child; `SlotRetiring(g, p)` |
| **Kill** | `SlotInert(g, p)` or `SlotActive(g, p)` | authenticated `kill` on the live handle, revocation, or drain | kill the child; `SlotRetiring(g, p)` |
| **StartFailed** | `SlotStarting(g, p, _)` | the launcher answers that the launch of `(slot, g)` produced no child and never will (COMP-5 custody class) | return occupancy (`need[k]`) and the start term to `residual`; return the right of `(slot, g)` (COMP-2); `SlotFree` |
| **Retire** | `SlotStarting(g, p, _)` (a child that ended before its start report), `SlotInert(g, p)`, `SlotActive(g, p)`, or `SlotRetiring(g, p)` | the launcher acknowledges the end of the child of `(host, run, slot, g)` with its descendants gone | return occupancy (`need[k]`, and the start term if still held) to `residual`; return the right of `(slot, g)` (COMP-2); `SlotFree`; remember a bounded last outcome per slot |
| **Handoff** | — | worker transfers shard ownership from current owner `A` to reachable team member `B` | atomically **Withdraw** every `SlotOffered` credit granted to `A`, advance `owner_epoch += 1`, set the active owner to `(B, owner_epoch, B's desk_session)`, and emit `AssumeOwner(shard, owner_epoch)` and fresh offers to `B` |

Every retirement event names `(host, run, slot, generation, principal)` and acts only on the slot
state with exactly that `generation`; any other event of these kinds is a no-op
(`RetireOnce`). Each Redeem's charge is therefore returned exactly once, by **StartFailed** or
**Retire**, and each Mint's hold exactly once, by **Withdraw**, **RedeemRefused**, or that slot's
**StartFailed** or **Retire**. These are local custody events, independent of billing.

### Dead dispatchers, partition handoff, and recovery bounds

- **Dead or stalled dispatchers.** The desk records outstanding grants per dispatcher connection
  and caps outstanding grants per connection and per principal. On disconnect, the desk clears that
  dispatcher's demand and sends `Abandon(slot, credit, desk_session)` to each affected worker; the
  worker Withdraws only if the slot is still `SlotOffered` with exactly that ordinal and session,
  and ignores `Abandon` otherwise. After redemption, withdrawal, or refusal, an idempotent
  settlement keyed by grant releases the connection's outstanding-grant allowance and nothing
  else. A create that fails caller authentication or names no live credit is dropped without
  touching any slot, so a guessed credit cannot burn an offer. A live but stalled dispatcher that stops redeeming is covered by the offer's bounded
  local hold (**Withdraw**). A dispatcher that dies after Redeem but before delivering an
  `effectful` handle leaves the slot `SlotInert`, which **InertExpire** reaps; a `pure` or
  `keyed` execution whose dispatcher died runs to completion and **Retire**s normally.
- **Desk restart.** A restarted desk member has a new `desk_session`. It accepts no offer bound
  to an earlier session; each worker that reconnects performs a new handshake, which resets its
  stream and Withdraws every offer it made to the earlier session, then mints fresh offers.
- **Exclusive-pop handoff under asymmetric partitions.** Under an asymmetric partition where
  worker `W` can reach team members `A` and `B`, `W`'s current owner is `A`, and dispatcher `D`
  can reach `B` but not `A`:
  1. Standby member `B` accepts `D`'s `Ask` (after `D` cannot reach `A` or exhausts its park
     interval) and signals unmet demand to eligible workers over bounded, authenticated demand
     subscriptions.
  2. While `W` holds unredeemed offers at `A` and sustained competing demand persists at
     reachable member `B` for a bounded local hold, `W` executes the **Handoff** transition
     above: `W` withdraws every unredeemed credit held by `A`, increments `owner_epoch`, and
     sends `AssumeOwner(shard, owner_epoch)` with fresh offers to `B`.
  3. **Who owns the pop and how `A` is fenced:** a desk member may execute the pop transition
     **only** for workers and `owner_epoch`s for which it holds an active `AssumeOwner` + live
     offers. Because `W` executes `Withdraw` on every `A`-held credit before emitting any offer
     to `B`, `A` is fenced at the worker (`WithdrawIsFinal`): any delayed grant `A` popped before
     observing `Withdraw` or stream reset fails `PlaceUnknownCredit` at `W` and can never redeem.
     On `A`, receiving `Withdraw` (or stream reset) drops `A`'s offer entries for `W`. Across the
     team, at most one desk member holds redeemable offers for any `(worker, slot)` at any time.
  4. When the partition heals and demand returns to the highest-ranked reachable member, the same
     symmetric **Handoff** returns `W`'s offers to `A`. Asks are versioned and cancelled on
     supersede so a delayed grant cannot resurrect an expired request. Safety follows strictly
     from serialized worker checks on the credit, independent of clock accuracy.
- **Liveness assumptions for bounded recovery.** The timed bounds in the [verification obligations](../../core/programs/fleet/COMPUTE-VERIFICATION.md) (`ReOffer`,
  `StrandBound`, `MintLatency`, `OrphanReclaim`) assume: (i) a continuously common reachable team
  member `B` for `W` and `D`, (ii) a working direct `D -> W` route, (iii) one-way message delay
  at most `delta`, (iv) local service delay at most `sigma` for any ready event at `W`, `B`, and
  `D` (a ready handoff, offer, or grant is processed within `sigma`), (v) detection delay at most
  `phi`, (vi) local offer hold `h` strictly above `2 * delta + 2 * sigma` (normal grant delivery
  and redemption), and (vii) for re-offer, funded capacity and a free right on `W` for an
  eligible waiting principal, with that demand persisting until the bound. A permanent partition
  intentionally strands rights (COMP-2); no bound applies across it.

### Placement misses and safety invariants

- **Placement miss disposition.** A create that exhausts placement without obtaining a grant
  terminates internally as **`PlacementMiss`**, distinct from a worker's
  **`WorkerCapacityRefusal`** or **`WorkerLimitRefusal`**. Its cause is the desk's last answer:
  `PlaceNoCredit` (no ready matching-class offer) or `PlaceNoRight` (ready matching-class offers,
  none with a right for the principal); a parked create whose deadline expires, or whose `Ask`
  goes unanswered for the dispatcher's local placement timeout, takes the cause of the last desk
  answer it saw, or `PlaceNoCredit` if it saw none. COMP-4 maps these to caller outcomes. In
  simulation and telemetry, a terminal `Refused(Capacity)` counts as a **false `Overloaded`** if
  and only if at least one physically idle, eligible, compatible slot with a free right for the
  principal existed in the cell at that terminal refusal.
- **Placement safety invariants (proved in Veil / checked in TLC and Quint, COMP-9):**
  - `OneLiveCredit`: for each slot, at most one credit can redeem, and only while `SlotOffered`.
  - `SingleUse`: a credit transitions a slot out of `SlotOffered` by Redeem at most once across
    all runs.
  - `OrdinalFresh`: no ordinal is minted twice in one `run`.
  - `GrantExclusive`: a desk member grants each offered credit to at most one dispatcher within
    its session, and at most one desk member holds redeemable offers for any `(worker, slot)` at
    any instant.
  - `WithdrawIsFinal`: once withdrawn (including during owner handoff or a refused Redeem), a
    credit never redeems.
  - `ActivateOnce`: user code holding grants starts at most once per `(run, slot, generation)`.
  - `RetireOnce`: each Redeem charge and each Mint hold is returned exactly once.

## COMP-4: Retry classes, attempt identity, and coordinator budget

### Three retry classes

Every create belongs to one of three retry classes, derived from the admitted release, its
transitive grant subset, and the actual contracts of every reachable sink — **never asserted by
the caller**:

```hirsel
pub type RetryClass: codec + key + send = RetryPure | RetryKeyed | RetryEffectful

pub type AttemptId: codec + key + send {
    caller_id: u64,
    principal_id: u64,
    session_id: u64,
    seq: u64,
    digest_lo: u64,
    digest_hi: u64
}

pub type CreateOutcome: codec + key + send =
    Created(u64)
    | Duplicate(u64)
    | RefusedCapacity
    | RefusedLimit
    | Undecided
    | SessionClosed
```

| Class | Condition | Behavior after an ambiguous send |
|---|---|---|
| **`pure`** | Empty grant subset, deterministic ([SEM-8](semantics.md#sem-8-determinism)), and duplicate-tolerant | May be re-dispatched on a fresh worker exchange within budget; duplicate work is invisible apart from physical occupancy until reclaimed. |
| **`keyed`** | Every reachable effectful sink enforces receiver-checked idempotency on a principal-scoped logical sub-operation ID and payload digest across workers and retries | May be re-dispatched within budget; every receiver atomically commits deduplication, effect, and replayable outcome, refusing a reused ID with a conflicting digest. |
| **`effectful`** | Everything else, including any execution holding a `Net` grant and any promised single interactive handle | **Created inert, then activated on the pinned handle.** Never re-dispatched on ambiguity. Pinned recovery queries or replays only the original worker and attempt, within the original invocation; if unresolved, returns `Undecided`. |

### Attempt identity and restart fence

Attempt identity is owned by the **caller**, distinct from any placement grant (COMP-3):

- Each logical attempt is bound to `(caller, principal, non-reusable session, sequence, request
  digest)` and, when dispatched to a worker, to the target worker's live `(host, run)`
  (COMP-6).
- **Sessions are bound to one coordinator instance.** A caller opens a session with one
  coordinator instance, and the session ends with it. The coordinator accepts each
  `(session, seq)` at most once, in increasing `seq`, and records the target it pins before its
  first worker send. An attempt naming a session the coordinator does not hold, or a `seq` not
  above the session's last, is answered `SessionClosed` without any send: it states nothing about
  any earlier attempt. So a second coordinator never receives retry authority over an earlier
  coordinator's attempt; the caller treats that earlier attempt as `Undecided` and opens a new
  session. This is the only
  retry authority across invocations: none exists outside the coordinator instance that holds the
  session.
- Within a worker `run`, the worker reserves deduplication state at admission and advances an
  explicit **retirement floor** per session (or irrevocably closes the session). An attempt
  arriving below the floor receives `LateUnknownId`
  ([FLT-7 K13](fleet.md#flt-7-proof-ready-protocol-kernel-contract-k1k13)), preventing eviction
  from allowing a delayed packet to launch twice.
- A repeated create carrying the same authenticated caller, target `(host, run)`, attempt
  identity, and digest replays its outcome (`Duplicate`); a conflicting digest is refused without
  running.
- **Restart fence for volatile attempt retirement:** because a volatile worker loses its
  per-session retirement floors on host-process restart, every worker-side dedup session is
  fenced to the worker `run` that opened it (COMP-5). After `host` restarts into a fresh
  `run' != run`:
  - any create or pinned-recovery request naming the old `run` (or carrying an old-`run`
    placement grant) is refused as `Gone(may_have_run: true)` / `LateUnknownId` and is **never**
    admitted as a fresh first create on `run'`;
  - for an **`effectful`** attempt whose send to `(host, run)` was unanswered, observing `run'`
    resolves to `Undecided` — the coordinator never re-binds that effectful attempt to `run'`;
  - for **`pure`** and **`keyed`** attempts, a retry opens a fresh attempt on a new target
    `(host', run')` (where `SEM-8` or the durable receiver's own principal-scoped idempotency table
    — not the volatile worker — enforces cross-restart deduplication).

### Inert effectful create and pinned activation

An `effectful` create prepares an **inert checkout** on the admitting worker without executing
user code that holds external grants. User code holding grants starts only on an authenticated
activation targeting the returned pinned handle, and activation is one-way: it moves the live
generation from `SlotInert` to `SlotActive` once, and every later activation of that generation
is answered without effect (COMP-3 **Activate**, **ActivateReplay**). A duplicate inert create
before activation therefore performs no external effect; it holds a slot, is accounted for, and
is reclaimed by **InertExpire**. The worker makes no durable write on this path. Once a send to
a worker is unanswered, no fresh worker is ever chosen for an `effectful` request; only a
definitive first-send refusal from the worker permits a new attempt within budget.

### Coordinator exchange budget and total outcomes

- **Three worker exchanges and one spill, in one invocation.** Each coordinator invocation
  receives a fixed budget of at most **3 worker exchanges** (counting failed sends, status
  lookups, and pinned replays; debited before send and never replenished on reconnect, partial
  batch, or progress) and at most **1 cross-cell spill** (COMP-8). The home invocation keeps sole
  control across a spill: it asks the remote cell's placement tier for a grant and sends to the
  remote worker itself, from the same budget, so no second coordinator and no second budget
  exist. Placement asks (COMP-3) have their own budget of at most **5** per invocation, debited
  before send: one for each worker exchange it funds with a grant (at most 3), plus one terminal
  miss in each of at most two cells (home and spill).
- **Finite local resolution.** Every exchange has a local timeout delivered as a `Timeout` event
  (K5): a placement `Ask` after the placement timeout `t_place`, a worker exchange after the
  exchange timeout `t_ex`. A create with no caller deadline is therefore resolved within
  `5 * t_place + 3 * t_ex` plus local service. A timed-out placement exchange is a
  `PlacementMiss`. A timed-out worker exchange whose create was sent is ambiguous: a local
  timeout ends the wait and never proves that the worker did nothing.
- **Coordinator death.** The coordinator is volatile. A coordinator that dies ends that internal
  invocation as `Undecided`, loses all authority to retry, and may drop its reply to the caller.
- **Total outcomes for a living coordinator (`TotalOutcome`).** Every living, scheduled
  coordinator invocation resolves to exactly one of `Created`, `Duplicate`, `Refused(Capacity)`,
  `Refused(Limit)`, or `Undecided` within budget and within the bound above. `SessionClosed` is
  not an invocation outcome: it answers a request that never became an invocation. Internal placement
  or worker replies (COMP-3 placement outcomes, `PlacementMiss`, `WorkerCapacityRefusal`,
  `WorkerLimitRefusal`, `Gone(may_have_run)`) are resolved by the dispatcher and never leak raw
  to callers.
- **Ambiguity is sticky (`OutcomeSoundness`).** Once any worker exchange of the invocation may
  have had an effect (an unanswered or timed-out create, or `Gone(may_have_run: true)`), the
  invocation can end only as `Created`, `Duplicate`, or `Undecided`; no later miss, refusal,
  reply, or spill downgrades it. `Refused(Capacity)` and `Refused(Limit)` are both returned only
  when every prior worker exchange is known to have had no effect (no send, or definitive
  first-send refusals on all sends).
- **What a refusal means.** Either refusal tells the caller that this logical attempt did not
  run and will not run. `Refused(Limit)` is returned when the terminal cause is a
  `WorkerLimitRefusal` or a `PlaceNoRight` miss: at that decision point, the worker or the desk
  held no unspent right of the principal that it could grant. It does not state that the
  principal's limit is exhausted cell-wide or fleet-wide. `Refused(Capacity)` is returned for a
  `WorkerCapacityRefusal` or a `PlaceNoCredit` miss.

## COMP-5: Restart custody and volatile scope

Compute kernels operate under the volatile scope clause **K11-V** of
[FLT-7](fleet.md#flt-7-proof-ready-protocol-kernel-contract-k1k13): a volatile ledger persists
nothing on the create path and releases directly only because every released execution either
dies with its ledger or is accounted for before its slot is reused. The rights assignment of
COMP-2 is a limit-right ledger and keeps K11's durable rule; it is written only when an escrow
table is installed.

The worker starts closed with a fresh `run`. It issues no credits until it receives an
empty-custody acknowledgement for the launcher session that fenced earlier requests. It
then rebuilds its spending ledger from its current assignments. An acknowledgement for
any other session leaves admission closed. Prior credits and sessions remain fenced by
the run identity (COMP-3, COMP-4).

## COMP-6: Checkout identity

A [checkout](../../core/programs/compute/protocol.hirsel) binds a credit, the principal
and the slot generation. The worker checks host, run, slot and generation against the
current credit and verifies that the slot is still occupied by the same principal.
A handle check or activation also requires authorization for that principal. Stale
or unauthorized checkouts confer no authority.

## COMP-7: Contained foreign modules

[FFI-10 through FFI-16](foreign.md#ffi-10-contained-modules-and-their-build) define the
contained foreign module boundary. The worker accounts for a class's declared capacity
vector under COMP-2.

## COMP-8: One-hop spill

- **One-hop spill (`SpillDepth <= 1`).** When local placement in the home cell cannot satisfy a
  create, the coordinator may spill **at most once** to one compatible remote cell. A spill is
  permitted only after the home placement tier answered `PlaceNoCredit` or `PlaceNoRight` for that
  create. The spilled request carries a `spilled` mark and its source cell, and a receiving cell
  never forwards a marked request again.
## COMP-9: Verification

The [Fleet placement simulator](../../core/programs/fleet/placement_sim.hirsel) exercises
the reducers. [Compute verification](../../core/programs/fleet/COMPUTE-VERIFICATION.md)
lists the safety obligations and placement oracles.
