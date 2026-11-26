# Compute

*Specified 2026-11-26 (Fleet Hyperscale Summit verdict, spec-only cutover, plus lane 3087
placement simulation gate `GATE=desk`; re-scoped 2026-11-26 as a reusable core scheduler). Rules
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
- **Separate QUIC endpoints.** Control traffic (`fleet@1` heartbeats, move commands, clearance,
  and `Directory[K]` lease renewals) and compute data-plane traffic run on **separate
  [QUIC](quic.md) endpoints** with separate drive tasks and reserved CPU and transport turns for
  control traffic. A saturating compute burst must never delay heartbeats, cause false suspicion,
  or trip [FLT-4](fleet.md#flt-4-scoped-takeover-barrier-membership-routing-and-drain)'s
  correlated-disruption breaker (`ControlLiveness`, COMP-9).

## COMP-2: Worker-only admission and the capacity ledger

The **worker** is the sole admission authority for compute execution, deciding on its full
physical resource vector with **zero physical overcommit**. Only the worker's admission ledger
can admit a slot or issue a physical host capacity refusal (`WorkerCapacityRefusal`; placement
routing misses are accounted distinctly in COMP-3 and COMP-4).

For every staged compute class on a host, admissible capacity is the exact minimum across all
per-host physical ceilings within `compute_pool`:

```text
capacity[class] = min(
    memory_slots(compute_pool.bytes, class),
    va_slots(compute_pool.va, class),
    warm_stock_slots(class),
    map_count_slots(class),
    fd_slots(compute_pool.fds, class),
    conn_table_slots(compute_pool.conns, class),
    start_concurrency_slots(class)
)
```

- **Full-vector atomic debit.** Multiple classes on one host can draw on the same physical bytes,
  address space, file descriptors, connection-table entries, CPU, or disk IOPS. Admission debits
  the full physical resource vector atomically across all dimensions before consuming moved input
  or starting execution; downstream exhaustion paths (unit stack mapping, Wasm `HostMemory`, or
  launcher descriptor exhaustion) become unreachable from compute.
- **Per-create checks at redemption.** Before converting a reservation into occupancy, the worker
  verifies caller authentication, principal authorization and grants, the principal's limit
  right (below), host cordon state,
  [FLT-6](fleet.md#flt-6-code-trust-revocation-clearance-transport-and-bucket-boundaries) release
  clearance and anti-rollback marks, grant subset, and full resource availability.
- **Lifetime boundaries.** Warm stock, in-flight starts, active
  occupants, orphans, and retiring children remain charged against the physical ledger until
  their matching termination or reset receipt retires them (`NoOverAdmit`). Start completion
  releases only the start-concurrency charge, never occupancy.
- **Shedding and control protection.** Under overload, a worker stops offering new capacity and
  sheds excess create traffic before expensive decode or allocation while protecting retirement,
  revocation, and control work. Limit refusals are reported distinctly from host saturation
  (`Refused(Limit)` vs. `Refused(Capacity)`).
### Hard per-principal concurrency limits

Each principal `p` has a concurrency limit: the number of its executions that may be live at
once. Limits are **hard**: zero overshoot, including during partitions, restarts, and desk
failover. The admission ledger enforces them with **escrowed rights**, not counters:

- **Disjoint escrow per worker.** A principal's allocation in a cell (COMP-8) is split into
  disjoint per-worker escrows with `sum_w escrow[p][w] <= allocation[p][cell]`. The split is an
  epoch-versioned escrow table, signed and installed per cell the same way as the federation
  manifest (COMP-8). To move rights between workers, the donor freezes the unspent rights
  being transferred, lowers its escrow and acknowledges the decrease. The recipient's escrow
  increases only after durable evidence of the donor's decrease. Spent rights cannot move.
- **Debit with the slot.** Admission debits one right from `escrow[p][w]` in the same atomic
  full-vector debit that funds the slot. A worker with no unspent right for `p` refuses before
  running (`WorkerLimitRefusal`, a definitive first-send refusal). Offers (COMP-3) carry which
  principals the worker still holds rights for, and the desk grants an offer to a create for
  `p` only from a worker holding a right for `p`.
- **Refund on the termination receipt.** A right returns to the worker's escrow only on the same
  termination or reset receipt that retires the slot (`NoOverAdmit`). Expiry, desk restart, or
  loss of contact never refunds it.
- **Stranded during a partition.** Rights held by an unreachable worker stay spent until that
  worker returns and accounts for its executions (COMP-5), or an operator terminates them through
  an audited action on the record.

`NoOverLimit`: for every principal `p` and cell `c`, the number of live admitted executions of
`p` in `c` never exceeds `allocation[p][c]`.

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

- **Worker.** Owns its physical slots and mints exact, single-use credits (or contiguous ordinal
  windows) only for already-funded capacity within a bounded per-class window `W`. Every credit
  is scoped to the worker's live process `run` (COMP-6) and cannot redeem in any later `run`.
- **Credit desk (shard team).** A soft-state rendezvous on fixed roster-ordered **desk teams**
  named in the cell's signed roster, with each class hashed to one shard/team. In steady state,
  the highest-ranked member of the shard's team is the shard primary and owns the pop transition
  for every connected worker. The desk holds offers and per-dispatcher, per-principal demand
  counts (`Ask(class, principal, n)`) — never payloads — and pops each offer to at most one
  dispatcher (`GrantExclusive`), choosing among waiting principals in proportion to their
  allocations. *(Team
  size, such as 3 hosts per team, is a non-normative deployment configuration parameter.)*
- **Dispatcher.** Runs in the trusted host process (never inside untrusted user code). Parks
  waiting creates within a bounded per-principal pending cap until a grant arrives or the caller's
  deadline expires; sends the create directly to the granted worker and returns the handle.
  Workers hold no queue. A create with no deadline receives an immediate `PlaceNoCredit` when the
  desk holds no matching offer.

```hirsel unbuilt
pub type CreditId: codec + key + send {
    worker_run: u64,
    lo: u64,
    hi: u64
}

pub type SlotState: codec + key + send =
    SlotFree
    | SlotOffered(CreditId, u64)
    | SlotBusy(CreditId, u64)

pub type PlacementReply: codec + key + send =
    PlaceUnknownCredit
    | PlaceCreditSpent
    | PlaceNoCredit
```

Worker transitions are serialized per worker and clock-free for safety. Each worker maintains a
monotone per-shard `owner_epoch: u64` and at most **one** active desk owner `(member,
owner_epoch)` per shard at any instant:

| Transition | Guard | Effect |
|---|---|---|
| **Mint** | Slot is `SlotFree`; in-flight offers/starts for the class are `< W`; worker is not cordoned or overloaded; full resource vector is funded | `SlotFree -> SlotOffered(c, owner_epoch)` where `c.worker_run == local_run`; emit `Offer(class, slot, c, owner_epoch)` to the worker's sole active desk owner |
| **Redeem** | `SlotOffered(c, _)` with `c.worker_run == local_run`, matching credit, and valid COMP-2 checks | `SlotOffered -> SlotBusy(c, generation)`; admit once (`OneLiveCredit`, `SingleUse`) |
| **Withdraw** | `SlotOffered(c, _)` on desk stream reset, `Abandon`, drain, overload, or owner handoff | `SlotOffered -> SlotFree`; credit `c` is permanently invalidated (`WithdrawIsFinal`) |
| **Handoff** | Worker transfers shard ownership from current owner `A` to reachable team member `B` | Atomically **Withdraw** every `SlotOffered` credit granted to `A` (fencing `A` via `WithdrawIsFinal`), advance `owner_epoch += 1`, set active owner to `(B, owner_epoch)`, and emit `AssumeOwner(shard, owner_epoch)` + fresh `Offer(..., owner_epoch)` to `B` |
| **Release** | `SlotBusy(c, generation)` after retirement receipts (COMP-5) | `SlotBusy -> SlotFree`; remember bounded last outcome per slot |

### Dead dispatchers, partition handoff, and recovery bounds

- **Dead or stalled dispatchers.** The desk records outstanding grants per dispatcher connection
  and caps outstanding grants per connection and per principal. On disconnect, the desk clears that
  dispatcher's demand and sends `Abandon(slot, c)` to each affected worker; the worker withdraws
  if still `SlotOffered(c, _)` and ignores `Abandon` if already `SlotBusy`. After redemption,
  withdrawal, or rejection, an idempotent settlement keyed by grant releases the connection's
  outstanding-grant allowance. For a live but stalled dispatcher that stops redeeming grants, the
  worker withdraws unredeemed offers after a bounded local hold.
- **Exclusive-pop handoff under asymmetric partitions.** Under an asymmetric partition where
  worker `W` can reach team members `A` and `B`, `W`'s current owner is `A`, and dispatcher `D`
  can reach `B` but not `A`:
  1. Standby member `B` accepts `D`'s `Ask` (after `D` cannot reach `A` or exhausts its park
     interval) and signals unmet demand to eligible workers over bounded, authenticated demand
     subscriptions.
  2. While `W` holds unredeemed offers at `A` and sustained competing demand persists at
     reachable member `B` for a bounded local hold, `W` executes the **Handoff** transition
     above: `W` withdraws every unredeemed credit held by `A`, increments `owner_epoch`, and
     sends `AssumeOwner(shard, owner_epoch)` with fresh offers `c'` to `B`.
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
     from serialized worker checks on `c`, independent of clock accuracy.
- **Explicit liveness assumptions for bounded recovery.** Bounded re-offer recovery requires:
  (i) a continuously common reachable team member `B` for `W` and `D`, (ii) a working direct
  `D -> W` route, (iii) funded capacity on `W`, (iv) fair service at `W`, `B`, and `D`, and
  (v) bounded detection delay, local hold interval (strictly exceeding normal grant delivery and
  redemption latency), and one-way message delays.

### Placement misses and safety invariants

- **Placement miss disposition.** A create that exhausts placement without obtaining a grant
  (such as an immediate no-deadline create receiving `PlaceNoCredit`, or a parked create whose
  deadline expires before a grant arrives) terminates internally as **`PlacementMiss`**, distinct
  from a worker's **`WorkerCapacityRefusal`**. Both map to caller-visible `Refused(Capacity)`
  only when no prior worker exchange may have had effects (COMP-4). In simulation and telemetry,
  a terminal `Refused(Capacity)` counts as a **false `Overloaded`** if and only if at least one
  physically idle, eligible, compatible slot existed in the cell at that terminal refusal.
- **Placement safety invariants (proved in Veil / checked in TLC and Quint, COMP-9):**
  - `OneLiveCredit`: for each slot, at most one credit can redeem, and only while `SlotOffered`.
  - `SingleUse`: a credit transitions a slot to `SlotBusy` at most once across all runs.
  - `GrantExclusive`: a desk member grants each offered credit to at most one dispatcher, and at
    most one desk member holds redeemable offers for any `(worker, slot)` at any instant.
  - `WithdrawIsFinal`: once withdrawn (including during owner handoff), a credit never redeems.

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
```

| Class | Condition | Behavior after an ambiguous send |
|---|---|---|
| **`pure`** | Empty grant subset, deterministic ([SEM-8](semantics.md#sem-8-determinism)), and duplicate-tolerant | May be re-dispatched on a fresh worker exchange within budget; duplicate work is invisible apart from physical occupancy until reclaimed. |
| **`keyed`** | Every reachable effectful sink enforces receiver-checked idempotency on a principal-scoped logical sub-operation ID and payload digest across workers and retries | May be re-dispatched within budget; every receiver atomically commits deduplication, effect, and replayable outcome, refusing a reused ID with a conflicting digest. |
| **`effectful`** | Everything else, including any execution holding a `Net` grant and any promised single interactive handle | **Created inert, then activated on the pinned handle.** Never re-dispatched on ambiguity. Pinned recovery queries or replays only the original worker and attempt; if unresolved, returns `Undecided`. |

### Attempt identity and restart fence

Attempt identity is owned by the **caller**, distinct from any placement grant (COMP-3):

- Each logical attempt is bound to `(caller, principal, non-reusable session, sequence, request
  digest)` and, when dispatched to a worker, to the target worker's live `(host, run)`
  (COMP-6).
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
activation targeting the returned pinned handle. A
duplicate inert create before activation therefore performs no external effect; it holds a slot,
is accounted for, and is reclaimed. Once a send to a worker is unanswered, no fresh worker is
ever chosen for an `effectful` request; only a definitive first-send refusal from the worker
permits a new attempt within budget.

### Coordinator exchange budget and total outcomes

- **Three worker exchanges and one spill.** Each coordinator invocation receives a fixed budget
  of at most **3 worker exchanges** (counting failed sends, status lookups, and pinned replays;
  debited before send and never replenished on reconnect, partial batch, or progress) and at most
  **1 cross-cell spill** (COMP-8). Placement-tier probes (COMP-3) are accounted separately from
  worker exchanges.
- **Coordinator death.** The coordinator is volatile. A coordinator that dies ends that internal
  invocation as `Undecided`, loses all authority to retry, and may drop its reply to the caller.
- **Total outcomes for a living coordinator (`TotalOutcome`).** Every living, scheduled
  coordinator invocation resolves to exactly one of `Created`, `Duplicate`, `Refused(Capacity)`,
  `Refused(Limit)`, or `Undecided` within budget and by its deadline. Internal placement or
  worker replies (COMP-3 placement outcomes, `PlacementMiss`, `WorkerCapacityRefusal`,
  `WorkerLimitRefusal`, `Gone(may_have_run)`) are resolved by the dispatcher and never leak raw to callers.
  `Refused(Capacity)` is returned only when every prior worker exchange is known to have had no
  outstanding effect (either a `PlacementMiss` before any worker send, or definitive first-send
  refusals on all prior sends).

## COMP-5: Restart custody (`AccountBeforeMint` by `ReapBeforeMint`) and volatile scope

Compute kernels operate under the volatile scope clause **K11-V** of
[FLT-7](fleet.md#flt-7-proof-ready-protocol-kernel-contract-k1k13): a volatile ledger persists
nothing on the create path and releases directly only because every released execution either
dies with its ledger or is accounted for before its slot is reused.

Because compute children are spawned by the launcher as siblings of the `platform` host process,
a host-process crash can leave compute children running while the volatile admission ledger is
lost. Compute therefore enforces **`AccountBeforeMint`**: a host advertises no capacity until
every surviving compute child has been accounted for. It meets that invariant by
**`ReapBeforeMint`**: every surviving child is killed, never re-adopted.

1. **Draw fresh `run` and start closed.** After any host-process restart, the worker draws a
   fresh, non-reusable `run` identifier (COMP-6), starts closed, and advertises or mints **zero**
   capacity. All placement credits and volatile dedup sessions from prior runs are irrevocably
   invalidated by the `run` mismatch (COMP-3, COMP-4), and pre-restart sessions stay closed
   (`LateUnknownId`).
2. **Fence old session.** At the launcher, fence the previous host session including any queued
   launch commands, so no in-flight spawn can complete after custody inspection.
3. **Reap every survivor.** Inspect the launcher's **compute custody class** only (never
   Archetype A Place runners or Archetype B serving children). Kill every surviving compute
   child and wait until its descendants are gone and the launcher acknowledges empty custody.
   Only then are its slots and principal limit rights (COMP-2) refunded and capacity advertised.
   If cleanup cannot be established, the successor remains closed.

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
- **Capacity accounting.** Virtual address space slots, MEM-19 resident bytes, and concurrently
  mounted async stacks are each terms of `capacity[class]` (COMP-2); hosts shard execution
  processes under the single physical host ledger when needed.

## COMP-8: Multi-cell federation and one-hop spill

A fleet scales horizontally across independent **cells**. Each cell owns its own `fleet@1`
control Place, enrolled roster, placement tier (COMP-3), cleared releases, revocation sequence,
and local `compute_pool`. A cell failure is contained to that cell.

- **Signed manifest of disjoint per-cell allocations.** There is no federation root Place.
  Federation state is a root-signed, epoch-versioned **federation
  manifest** listing cells, their SPKI pins, residency tags, and disjoint per-cell principal
  allocations satisfying `sum_c allocation[p][c] <= limit[p]`. Each cell installs the
  manifest through its existing `fleet@1` root-signed keyset and durable anti-rollback path
  ([FLT-6](fleet.md#flt-6-code-trust-revocation-clearance-transport-and-bucket-boundaries)),
  making manifest installation durable and K11-covered per cell. Inside a cell, the allocation is
  enforced by hard worker escrow (COMP-2).
- **Decrease before increase.** Shifting allocation from donor cell `X` to recipient cell `Y`
  requires two signed manifests: the first lowers `X` and waits for durable evidence/receipt that
  `X` has relinquished the moved rights; the second raises `Y`. `sum <= limit` holds at
  every intermediate state. Rebalancing is an external operator or program tool emitting signed
  manifests.
- **One-hop spill (`SpillDepth <= 1`).** When local placement in the home cell cannot satisfy a
  create, the coordinator may spill **at most once** to one compatible remote cell. The spilled
  request carries a `spilled` mark, and a receiving cell never forwards a marked request again.
- **Summary picks the cell; remote placement decides.** Cells exchange fresh, quantized
  per-cell/class headroom summaries. The home coordinator uses those summaries (filtered by
  residency, principal limit, release clearance, and grant policy) to pick one target cell,
  then sends an exact placement request to that cell's placement tier (COMP-3). A stale summary
  costs at most an exact placement refusal, never a broadcast launch storm.
- **Destination protection.** Receiving cells serve local cell demand before spilled demand and
  enforce inbound per-source, per-class, per-principal, and aggregate spill caps. Sending cells cap
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
model-based testing on `SM1` traces):

| Component | Required safety invariants |
|---|---|
| Worker admission & custody | `NoOverAdmit`, `NoOverLimit`, `AccountBeforeMint` (by `ReapBeforeMint`), `HandleFresh`, attempt-identity deduplication, `LateUnknownId` retirement floor, and cross-restart `run` fencing (COMP-4, COMP-5) |
| Placement & dispatcher | Placement invariants defined in COMP-3 (`OneLiveCredit`, `SingleUse`, `GrantExclusive`, `WithdrawIsFinal`); dispatcher `TotalOutcome` (living coordinator within 3 worker exchanges and 1 spill), `OutcomeSoundness`, `EffectfulNeverRedispatched` |
| Federation and escrow | `SpillDepth <= 1`, `sum_c allocation[p][c] <= limit[p]` across every manifest step, `sum_w escrow[p][w] <= allocation[p][c]` across every escrow table step |
| Non-interference | Build-time check: compute modules import neither `OutboxCmd` nor `Key` from `core/programs/fleet/types.hirsel` |
| Contained Wasm | Reference-entry differential (CoW-off oracle) before any pooled/CoW optimization |

Timed assume/guarantee liveness properties (`OrphanReclaim`, `StrandBound`, `MintLatency`, and
asymmetric-partition re-offer recovery) are proved separately from clock-free safety kernels
([FLT-7 K7](fleet.md#flt-7-proof-ready-protocol-kernel-contract-k1k13)).

### Statistical placement laws (1k, 10k, and 100k logical workers)

Placement quality is checked statistically across paired seeds and open-loop arrivals in one
logical-worker driver extending lane 3056's Fleet simulator (no second simulator), against exact
independent oracles. Fault-free closed-form oracles require their stated preconditions; crash,
message-delay, and partition schedules are verified as separate fault/liveness cases:

- **Burst bound (deterministic oracle).** *Preconditions:* a homogeneous eligible class with
  reachable workers and shard primary, dispatcher pending caps `>= N`, all `f_i` free slots
  already funded and offered (or bounded mint/grant/delivery RTT `<= r`), per-slot start service
  time `<= s`, window `W`, caller deadlines `>= t`, no binding principal limit, fair service, no
  failures, and no slot releases within `[0, t]`. *Oracle:* creates started by time `t` are at
  least `min(N, sum_i min(f_i, W * floor(t / (s + r))))`. Includes **zero-slack conservation**:
  `N = sum f_i`, `W = 1`, `deadline = max_i f_i * (s + r)` with zero creates left unstarted.
- **Immediate creates (deterministic oracle).** *Preconditions:* `N` no-deadline creates for one
  eligible class arriving at a reachable placement tier currently holding `q` ready matching-class
  offers, with no binding principal limit, fair service, and no failures. *Oracle:* exactly
  `min(N, q)` are granted and the remaining `max(0, N - q)` terminate as `PlacementMiss`
  (`Refused(Capacity)`).
- **Steady-state blocking (Erlang-B statistical oracle).** *Preconditions:* a homogeneous
  single-class pooled loss system with `C` total funded slots, open-loop Poisson arrival rate
  `lambda`, mean occupancy `1 / mu` (offered load `A = lambda / mu` Erlangs), no waiting
  (no-deadline arrivals), no binding principal limit, and no failures. *Oracle:* **Erlang-B**
  `B(C, A)` gives the exact blocking lower bound; simulated refusal probability must match
  `B(C, A)` within the stated excess attributable to offer window `W` and in-flight grant RTT,
  with false `Overloaded` tracked separately (COMP-3).
- **Separate fault, fairness, overload, and control-liveness cases.** Asymmetric-partition
  stranded-slot re-offer within the stated recovery bound under COMP-3's liveness assumptions,
  `ControlLiveness` (zero missed A/B heartbeat or `Directory[K]` renewal deadlines under
  saturating compute bursts), principal max-min fairness, zero spill while home capacity is
  available, and metastability recovery after excess load ends.

## Dependencies

Live `compute@1` implementation depends on:

- **`platform-fleet-seams-3081`:** enrolled node identity and the signed, epoch-versioned cell
  roster carrying `place_reserve` / `compute_pool` budgets and shard placement membership
  (COMP-3).
- **Pinned QUIC links:** Fleet Phase 3b's join, roster, and SPKI-pinned QUIC link layer, split
  from Phase 3b's Archetype A move verbs.

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
   partition, refunded only on termination receipts (COMP-2).
