# Compute

*Specified 2026-11-26 (Fleet Hyperscale Summit verdict, spec-only cutover, plus lane 3087
placement simulation gate `GATE=desk`; re-scoped 2026-11-26 as a reusable core scheduler;
amended 2026-11-26 by lane 3123 to close astra attack 3119). Rules
COMP-1 to COMP-9. Not built; all Hirsel type
sketches carry `unbuilt` fences. Archetype C compute placement lives exclusively in this
specification and never inside `fleet@1`
([FLT-1](fleet.md#flt-1-three-workload-archetypes),
[FLT-3](fleet.md#flt-3-control-place-fleet1-and-controller-takeover)).*

Hirsel Compute (`compute@1`) is the core scheduler for Archetype C elastic remote workloads:
native units and contained Wasm tasks, run inside the existing single-binary `hirsel` launcher
and `platform` host. It is a reusable part of the Hirsel ecosystem that any program or service
can use to run work on enrolled hosts. Workers alone admit work against real host resources;
`fleet@1` never tracks compute slots or sits on the create path.

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
  of its executions can still run. (As with Kubernetes' out-of-service node taint, the operator
  asserts a shutdown; it is not inferred from silence.)

`NoOverLimit`: for every principal `p` and cell `c`, the number of live admitted executions of
`p` in `c` never exceeds the effective `allocation[p][c]` (COMP-8). It follows from `EscrowBalance` and
`EscrowConservation`.

## COMP-3: Placement (credit desk — decided)

*Status: **decided (`GATE=desk`)** by the lane 3087 placement simulation gate (52 paired runs at
1k, 10k, and 100k logical workers extending lane 3056's simulator: worst credit-desk false
`Overloaded` `0.0065%` vs. broadcast `4.155%` through `rho = 0.5`; asymmetric-partition re-offer
`6.150 ms` vs. `13.100 ms`; `0` double redemptions).*

Placement matches exact worker-funded capacity to demand at **one exclusive rendezvous per
shard** — the **credit desk** (equivalently, an exclusive ordinal pop at the shard relay, with
one active owner per worker-shard offer). All placement-specific roles, types, roster fields,
and safety invariants are encapsulated in this section.

### Deleted mechanisms

The following mechanisms are explicitly absent from `compute@1` and must not be built:

- the Archetype C capacity-report picker in `fleet@1` or ingress;
- any broadcast two-choice placement path or stale-view fallback beside credits;
- epsilon overcommit on offers;
- any second placement route for in-fleet callers (every create takes the same placement path).

### Placement latency cost

Each create adds one dispatcher-to-desk round trip. The 3087 gate measured `+100 us` p50 at
`100 us` desk RTT.

### Roles and exclusive credit protocol

Three soft-state roles run inside the single `hirsel` binary:

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
  its current one. Among waiting principals it chooses by weighted fair queueing with weights
  `allocation[p][cell]`: the shard keeps a virtual time `V` and each principal a finish tag. The
  desk grants the waiting eligible principal with the smallest
  `start[p] + 1 / allocation[p][cell]`, where `start[p] = max(finish[p], V)`, ties to the lowest
  principal ID; the grant sets `finish[p]` to that value and `V` to it. `V` is the tag of the
  last grant at the shard, so a principal returning from idle starts at `V` and gains no credit
  for its idle time. Tags are exact rationals (pairs of integers, FLT-7 K4), never floats
  (self-clocked fair queueing, Golestani 1994, which gives the bound in COMP-9). *(Team size, such
  as 3 hosts per team, is a non-normative deployment configuration parameter.)*
- **Dispatcher.** Runs in the trusted host process (never inside untrusted user code). Parks
  waiting creates within a bounded per-principal pending cap until a grant arrives or the caller's
  deadline expires; sends the create directly to the granted worker and returns the handle.
  Workers hold no queue. A create with no deadline receives an immediate `PlaceNoCredit` when the
  desk holds no matching-class offer, and `PlaceNoRight` when it holds matching-class offers but
  none carries a right for the create's principal.

```hirsel unbuilt
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
- **Liveness assumptions for bounded recovery.** The timed bounds of COMP-9 (`ReOffer`,
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

```hirsel unbuilt
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

## COMP-5: Restart custody (`AccountBeforeMint` by `ReapBeforeMint`) and volatile scope

Compute kernels operate under the volatile scope clause **K11-V** of
[FLT-7](fleet.md#flt-7-proof-ready-protocol-kernel-contract-k1k13): a volatile ledger persists
nothing on the create path and releases directly only because every released execution either
dies with its ledger or is accounted for before its slot is reused. The rights assignment of
COMP-2 is a limit-right ledger and keeps K11's durable rule; it is written only when an escrow
table is installed.

Because compute children are spawned by the launcher as siblings of the `platform` host process,
a host-process crash can leave compute children running while the volatile admission ledger is
lost. Compute therefore enforces **`AccountBeforeMint`**: a host advertises no capacity until
every surviving compute child has been accounted for. It meets that invariant by
**`ReapBeforeMint`**: every surviving child is killed, never re-adopted.

1. **Draw fresh `run` and start closed.** After any host-process restart, the worker draws a
   fresh, non-reusable `run` identifier (COMP-6), starts closed, and advertises or mints **zero**
   capacity. All placement credits and volatile dedup sessions from prior runs are irrevocably
   invalidated by the `run` mismatch (COMP-3, COMP-4), and pre-restart sessions stay closed
   (`LateUnknownId`). A worker whose current signed roster does not list its host stays closed.
2. **Fence every predecessor.** The host opens a new launcher control session (`Hello`, RUNNER-SPEC
   §14.5 "The control session"). Opening it ends every earlier session, and every launch request
   of an earlier session that the launcher had not acted on is discarded and never runs. This
   covers every predecessor `run`, not only the previous one. The worker then cancels each
   compute launch that the session's inventory lists as accepted and not yet started, and waits
   for the launcher to confirm each cancellation.
3. **Reap every survivor.** Inspect the launcher's **compute custody class** only (never
   Archetype A Place runners or Archetype B serving children). Kill every surviving compute
   child and wait until its descendants are gone and the launcher acknowledges, **in the session
   opened at step 2**, that compute custody is empty with no accepted launch. An acknowledgement
   from any other session is ignored. Only then does the worker rebuild its spending ledger from
   its durable assignment (`free[p] = assigned[p]`, `charged[p] = {}`, `frozen[p] = 0`, all
   holds returned) and advertise capacity. If the recovery is interrupted, the next `run`
   repeats steps 1 to 3 with its own session. If cleanup cannot be established, the successor
   remains closed.

The compute custody class makes these launcher answers definitive:

- Compute children run in delegated cgroups named by child identity, so a launcher-loss successor
  empties them before it answers `Hello` (RUNNER-SPEC §14.5 "A launcher loss"). A host whose
  launcher runs without cgroup delegation cannot establish custody after a launcher loss and
  stays closed.
- Every compute launch the launcher accepts ends with exactly one of: a start report, then an end
  acknowledgement (COMP-3 **Started**, **Retire**); or a **no-child** answer stating that the
  launch was refused or cancelled before any child existed and will never start (COMP-3
  **StartFailed**).

An in-flight `effectful` attempt on the old `run` resolves to `Undecided` (COMP-4); the reap
guarantees it is no longer running once the successor opens.

## COMP-6: Opaque, versioned handles

Every running compute execution is addressed by an **opaque, versioned handle**:

```hirsel unbuilt
pub type ComputeHandle: codec + key + send {
    version: u8,
    cell: u16,
    host: u32,
    run: u64,
    slot: u32,
    generation: u64,
    secret: Bytes
}
```

- **Opaque and versioned.** Callers treat handles as opaque tokens; the leading `version` tag
  governs wire format evolution without exposing internal layout to callers.
- **Internal tuple and freshness.** Internally the handle carries `(cell, host, run, slot,
  generation)` plus an **authenticated secret**. `run` is a fresh per-host-process-start value
  (also fencing pre-restart placement credits and attempt sessions, COMP-3–COMP-5), and `generation` is a non-wrapping per-slot sequence (refusing counter wrap).
  Together with a fresh cryptographic boot key, `(run, slot, generation)` derives a
  domain-separated authenticated secret whose wire width is sized to the fleet-lifetime
  collision/forgery risk budget before freezing the wire format.
- **No `fleet@1` node incarnation.** The durable node incarnation stored in `fleet@1`
  ([FLT-2](fleet.md#flt-2-move-protocol-claimauth-origin-superseded-settlement-deposed),
  [FLT-3](fleet.md#flt-3-control-place-fleet1-and-controller-takeover)) is **not** part of the
  compute handle. Compute fencing never requires a `fleet@1` write on host-process restart.
- **Rights checked on every use (`HandleFresh`).** Routing fields (`cell`, `host`) confer no
  authority. Every operation on a handle (`activate`, `exec`, `signal`, `kill`, `status`)
  authenticates the caller and verifies principal identity, current grants, live `(run, slot,
  generation)`, and the authenticated secret. Host identifiers are never reused within a fleet:
  a known retired slot, `run`, or host returns `Gone`, while an unreachable owner returns
  `Undecided` (neither settles prior external effects).

## COMP-7: Contained Wasm pricing and slot rules

Contained Wasm compute tasks obey
[FFI-10 through FFI-16](foreign.md#ffi-10-contained-modules-and-their-build) with zero divergence
in observable results, step counts, failure order, or replay tapes:

- **Copy-on-write is off.** Contained Wasm runs with copy-on-write initialization disabled on the
  pinned Wasmtime engine.
- **Fresh `Store` per task.** Every task receives a fresh `Store` and fresh handles as its
  isolation boundary; globals, tables, handles, and fuel never survive across tasks.
- **Vacant slot pooling and full reservation.** Pooled vacant backing slots retain the **full
  memory reservation** (4 GiB address space plus 32 MiB guard per linear memory on the pinned
  engine). A release's declared memory limit is never shrunk silently
  ([FFI-13](foreign.md#ffi-13-instances-bounds-and-steps),
  [FFI-14](foreign.md#ffi-14-determinism-traps-and-replay)); pool classes are keyed so every
  module runs in a class covering its declared limit. **Reusing pooled slots across tasks is held
  pending the security and isolation audit** proving zero stale memory, table, global, or handle
  observations.
- **Reference-entry `inst_steps` pricing (no closed-form price).** Instantiation and optional
  guest `_initialize` are priced strictly by the **pinned reference entry point** (CoW off),
  preserving exact first-call failure and slice order:
  1. initial memory admission against the caller's current [MEM-19](budgets.md#mem-19-limits)
     bytes limit before any fuel is consumed;
  2. active segment out-of-range traps;
  3. metered fuel granules and [MEM-30](budgets.md#mem-30-step-bounds) slice-end positions;
  4. live metered guest `_initialize` (including any `memory.grow` against current bytes limits)
     and exact trap-versus-`OutOfTime` ordering.

  **No closed-form tariff** (such as data-segment byte length) is used, and no automatic fallback
  between meters is permitted.
- **Capacity accounting.** Virtual address space, MEM-19 resident bytes, and concurrently
  mounted async stacks are each dimensions of the COMP-2 residual vector, demanded by each
  contained Wasm class; hosts shard execution processes under the single physical host ledger
  when needed.

## COMP-8: Multi-cell federation and one-hop spill

A fleet scales horizontally across independent **cells**. Each cell owns its own `fleet@1`
control Place, enrolled roster, placement tier (COMP-3), cleared releases, revocation sequence,
and local `compute_pool`. A cell failure is contained to that cell.

- **Signed manifest of disjoint per-cell allocations.** There is no federation root Place.
  Federation state is a root-signed, epoch-versioned **federation
  manifest** listing cells, their SPKI pins, residency tags, principal limits `limit[p]`, and
  disjoint per-cell principal allocations satisfying `sum_c allocation[p][c] <= limit[p]`. Each
  cell installs the manifest through its existing `fleet@1` root-signed keyset and durable
  anti-rollback path
  ([FLT-6](fleet.md#flt-6-code-trust-revocation-clearance-transport-and-bucket-boundaries)),
  making manifest installation durable and K11-covered per cell. The manifest signer keeps its
  chain as the rights authority does (COMP-2): exactly one successor per manifest, recorded
  durably before release. Inside a cell, the allocation is enforced by the escrow table and
  worker escrow (COMP-2).
- **Decrease before increase, one level up.** The manifest follows COMP-2's rule: each manifest
  only lowers or only raises, and a manifest that raises a cell's allocation (or a principal's
  limit) is signed only after every lowering before it has been acknowledged. Cell `X`
  acknowledges a lowering of `allocation[p][X]` only after its rights authority has signed an
  escrow table whose `sum_w assigned[p][w]` fits the new allocation and every worker that table
  lowered has acknowledged it (COMP-2 donor surrender). The acknowledgement names the manifest
  epoch and backs exactly one raising manifest, which is replayed, never re-decided. Lowering
  `limit[p]` first lowers allocations, acknowledged, and then signs the lower limit.
  A cell's **effective allocation** `allocation[p][c]` is the value its installed manifest gives,
  except that a lowering takes effect only at the cell's acknowledgement (until then the
  effective value is the previous, higher one) and a raise takes effect when the cell installs
  it. COMP-2 and COMP-9 state `EscrowConservation` and `NoOverLimit` against the effective
  allocation, and `sum_c allocation[p][c] <= limit[p]` holds over effective allocations at every
  intermediate state (`AllocationConservation`). Rebalancing is an external operator or program
  tool emitting signed manifests.
- **One-hop spill (`SpillDepth <= 1`).** When local placement in the home cell cannot satisfy a
  create, the coordinator may spill **at most once** to one compatible remote cell. A spill is
  permitted only after the home placement tier answered `PlaceNoCredit` or `PlaceNoRight` for that
  create. The spilled request carries a `spilled` mark and its source cell, and a receiving cell
  never forwards a marked request again.
- **Summary picks the cell; remote placement decides.** Cells exchange fresh, quantized
  per-cell/class headroom summaries. The home coordinator uses those summaries (filtered by
  residency, principal limit, release clearance, and grant policy) to pick one target cell,
  then sends an exact placement request to that cell's placement tier (COMP-3) and, on a grant,
  sends the create to the granted remote worker from its own budget (COMP-4). A stale summary
  costs at most an exact placement refusal, never a broadcast launch storm.
- **Destination protection.** Receiving cells serve local cell demand before spilled demand and
  enforce inbound per-source, per-class, per-principal, and aggregate spill caps at their
  placement tier, which sees every spilled `Ask` with its source cell. Sending cells cap
  outbound spill requests and bytes and suppress spill when fresh remote headroom is absent or
  local observer health is suspect.
- **Cross-cell handle resolution.** Handles carry `(cell, host)` resolved via the manifest and
  target cell roster; no limit right crosses a cell boundary at run time.

## COMP-9: Verification and proof split

Compute follows [FLT-7](fleet.md#flt-7-proof-ready-protocol-kernel-contract-k1k13) pure reducer
kernels (`step(state, ev)` and bounded `keys(ev)`, K1–K13 with K11-V) and
[FLT-8](fleet.md#flt-8-verification-tiers-t0t3-and-sm1-traces) verification tiers T0–T3, split
strictly between deterministic safety proofs and statistical placement laws:

### Deterministic safety proofs (Veil for unbounded requests; TLC and Quint over SM1)

Worker invariants mention one worker with unbounded requests and placement grants, and are proved
inductively in **Veil** for any number of requests (cross-checked by bounded TLC and Quint
model-based testing on `SM1` traces). Each invariant names the rule it follows from:

| Component | Invariant | Statement | Follows from |
|---|---|---|---|
| Worker admission | `NoOverAdmit` | `residual[d] >= 0` for every dimension; every hold, occupancy, and start term is charged until its single return | COMP-2 physical ledger; COMP-3 slot machine |
| Worker admission | `EscrowBalance` | `free[p] + count(charged[p]) + frozen[p] = assigned[p]` at every stable state of a run | COMP-2 spending ledger, donor surrender |
| Worker admission | `NoOverLimit` | live executions of `p` in cell `c` `<=` effective `allocation[p][c]` | `EscrowBalance` and `EscrowConservation` |
| Worker custody | `RetireOnce` | each Redeem charge and each Mint hold is returned exactly once, by the event matching `(host, run, slot, generation, principal)` | COMP-3 retirement table |
| Worker custody | `ActivateOnce` | user code holding grants starts at most once per `(run, slot, generation)` | COMP-3 **Activate**; COMP-4 inert create |
| Worker custody | `AccountBeforeMint` (by `ReapBeforeMint`) | no Mint in a `run` before the empty-custody acknowledgement of that run's own launcher session | COMP-5 |
| Worker custody | `HandleFresh` | every handle operation acts only on the live `(run, slot, generation)` with a valid secret | COMP-6 |
| Attempts | attempt deduplication, `LateUnknownId` | a repeated attempt replays its outcome; one below the retirement floor never runs | COMP-4 attempt identity |
| Attempts | `RunFence` | no attempt, credit, or handle of an earlier `run` acts in a later one | COMP-4 restart fence; COMP-5 step 1 |
| Placement | `OneLiveCredit`, `SingleUse`, `OrdinalFresh`, `GrantExclusive`, `WithdrawIsFinal` | as stated in COMP-3 | COMP-3 credit protocol, desk sessions |
| Dispatcher | `TotalOutcome` | every living invocation ends in exactly one outcome within 3 worker exchanges, 5 placement asks, 1 spill, and `5 * t_place + 3 * t_ex` | COMP-4 budget and finite local resolution |
| Dispatcher | `OutcomeSoundness` | `Refused(*)` only if no exchange may have had an effect; ambiguity is never downgraded | COMP-4 ambiguity is sticky |
| Dispatcher | `EffectfulNeverRedispatched` | after an ambiguous send of an `effectful` attempt, no create for it is sent to any other worker; only its original worker is queried or replayed, and only by the coordinator instance holding its session | COMP-4 effectful class, sessions bound to one coordinator |
| Federation and escrow | `EscrowConservation` | `sum_w assigned[p][w]` at each worker's installed epoch `<=` effective `allocation[p][c]` | COMP-2 decrease then increase, one acknowledgement per increase |
| Federation and escrow | `TransferOnce` | each transfer's rights are applied by its recipient at most once and never restored to its donor | COMP-2 in transit until applied |
| Federation and escrow | `AllocationConservation` | `sum_c` effective `allocation[p][c]` `<= limit[p]` | COMP-8 decrease before increase |
| Federation and escrow | `SpillDepth <= 1` | no create is placed by more than two placement tiers | COMP-8 one-hop spill |
| Non-interference | boundary check | compute modules import neither `OutboxCmd` nor `Key` from `core/programs/fleet/types.hirsel` (build-time) | COMP-1 |
| Contained Wasm | reference-entry equivalence | results, step counts, failure order, and replay tapes equal the pinned reference entry with CoW off | COMP-7 |

Timed assume/guarantee liveness properties are proved separately from clock-free safety kernels
([FLT-7 K7](fleet.md#flt-7-proof-ready-protocol-kernel-contract-k1k13)), under the COMP-3
liveness assumptions (`delta`, `sigma`, `phi`, `h`):

| Property | Start event | End event | Extra assumptions | Bound |
|---|---|---|---|---|
| `MintLatency` | a slot becomes `SlotFree` (or a class's incremental vector starts to fit) | its `Offer` is ready at the active desk owner | worker not cordoned, overloaded, or closed; fewer than `W` in-flight offers and starts | `sigma + delta + sigma` |
| `StrandBound` | an offer at a desk becomes unredeemable (its desk or the dispatcher it was granted to is lost) | the slot is `SlotFree` again | the worker stays up | `h + sigma` |
| `ReOffer` | competing demand for `W`'s class appears at reachable member `B` while `W`'s offers sit at unreachable `A` | a fresh offer from `W` is ready at `B` | (i)–(vii) of COMP-3 | `phi + h + 2 * (sigma + delta)` |
| `OrphanReclaim` | an `effectful` slot becomes `SlotInert` and its handle is never delivered | the slot's charge and occupancy are returned (**Retire**) | the launcher acknowledges a child's end within `k_end` of its kill | `a + sigma + k_end`, with `a` the activation hold |

### Statistical placement laws (1k, 10k, and 100k logical workers)

Placement quality is checked statistically across paired seeds and open-loop arrivals in one
logical-worker driver extending lane 3056's Fleet simulator (no second simulator), against exact
independent oracles. Fault-free closed-form oracles require their stated preconditions; crash,
message-delay, and partition schedules are verified as separate fault/liveness cases:

- **Burst bound (deterministic oracle).** *Preconditions:* a homogeneous eligible class with
  reachable workers and shard primary, dispatcher pending caps `>= N`, per-connection and
  per-principal outstanding-grant allowances `>= N`, start concurrency per worker `>= W`, all
  `f_i` free slots already funded and offered (or bounded mint/grant/delivery RTT `<= r`),
  per-slot start service time `<= s`, window `W`, caller deadlines `>= t`, no binding principal
  limit, fair service, no failures, and no slot releases within `[0, t]`. *Oracle:* creates
  started by time `t` are at least `min(N, sum_i min(f_i, W * floor(t / (s + r))))`. Includes
  **zero-slack conservation**: `N = sum f_i`, `W = 1`, `deadline = max_i f_i * (s + r)` with zero
  creates left unstarted.
- **Immediate creates (deterministic oracle).** *Preconditions:* `N` no-deadline creates for one
  eligible class all arrive at a reachable placement tier holding `q` ready matching-class offers
  with rights for their principals, within an observation interval in which no offer is minted,
  withdrawn, or expires, no other create arrives, and per-connection and per-principal
  outstanding-grant allowances are `>= N`; no binding principal limit, fair service, and no
  failures. *Oracle:* exactly `min(N, q)` are granted and the remaining `max(0, N - q)` terminate
  as `PlacementMiss` (`Refused(Capacity)`).
- **Steady-state blocking (Erlang-B statistical oracle).** *Preconditions:* a homogeneous
  single-class pooled loss system with `C` total funded slots, open-loop Poisson arrival rate
  `lambda`, mean occupancy `1 / mu` (offered load `A = lambda / mu` Erlangs), no waiting
  (no-deadline arrivals), no binding principal limit, no failures, and a maximum gap `d` from a
  slot's release to its next redeemable offer plus grant delivery (`d <= MintLatency + delta +
  sigma`), which the driver holds fixed and independent of system state. *Oracle:* a slot is unavailable for its occupancy plus at most `d`, and Erlang-B is
  insensitive to the holding-time distribution, so the refusal probability lies in
  `[B(C, A), B(C, lambda * (1 / mu + d))]`. The simulated refusal rate must fall inside that
  interval widened by its 99% binomial confidence interval over the paired seeds. False
  `Overloaded` is tracked separately (COMP-3).
- **Weighted fairness (deterministic oracle).** *Preconditions:* principals `p` and `q`
  continuously waiting and eligible at one shard over an interval, unit of service one grant.
  *Oracle:* `|g_p / allocation[p] - g_q / allocation[q]| <= 1 / allocation[p] + 1 /
  allocation[q]`, where `g` counts grants in the interval (the self-clocked fair queueing bound
  of COMP-3's desk choice). With allocations 10 and 30, four contested grants split 1:3.
- **Zero spill while home capacity is available.** *Oracle:* no spill follows a home placement
  decision at whose instant the home desk held a ready matching offer with a right for the
  principal. Offers in flight to the desk are not ready.
- **`ControlLiveness`.** *Preconditions:* control deadlines carry at least `d_ctl` of slack
  (COMP-1). *Oracle:* zero missed A/B heartbeat or `Directory[K]` renewal deadlines under
  saturating compute bursts.
- **Separate fault, overload, and recovery cases.** Asymmetric-partition re-offer within
  `ReOffer`, stranded offers within `StrandBound`, unclaimed inert checkouts within
  `OrphanReclaim`, and metastability recovery after excess load ends.

## Dependencies

Live `compute@1` implementation depends on:

- **`platform-fleet-seams-3081`:** enrolled node identity and the signed, epoch-versioned cell
  roster carrying `place_reserve` / `compute_pool` budgets and shard placement membership
  (COMP-3).
- **Pinned QUIC links:** Fleet Phase 3b's join, roster, and SPKI-pinned QUIC link layer, split
  from Phase 3b's Archetype A move verbs.
- **Launcher compute custody class:** delegated cgroups for compute children, cancellation of an
  accepted launch with a confirmation, the definitive no-child answer, and the empty-custody
  acknowledgement bound to the current control session (COMP-5). `launch.md` and RUNNER-SPEC
  §14.5 supply the session fence these build on, not the class itself.

**Not a dependency:** lane **3048** (`ClaimAuth` + `ORIGIN` Store move fencing). Compute
performs no Place claim and consumes neither `ClaimAuth` nor `ORIGIN`.

*Related routed prerequisites (tracked outside this spec):* indexed QUIC endpoint free-slot
stack, generation handles, deadline heap, and ready queue (lane 3060, landed); fixed-roster
pointwise K10 advisory merge (lane 3056, landed); and the placement simulation gate extending
lane 3056's simulator (lane `fleet-placement-sim-gate-3087`, decided `GATE=desk`).

## Rulings

Rulings of 2026-11-26 (Sam re-scoped compute to a reusable core scheduler; both reversible):

1. **Host-process restart reaps.** In-flight compute is killed and accounted for before capacity
   is re-advertised (`ReapBeforeMint`, COMP-5).
2. **Principal limits are hard.** Escrowed rights at workers, zero overshoot, stranded during a
   partition, and returned only by the retirement that matches their charge (COMP-2, COMP-3).
