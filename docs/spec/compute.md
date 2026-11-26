# Compute

*Specified 2026-11-26 (Fleet Hyperscale Summit verdict, spec-only cutover, plus lane 3087
placement simulation gate `GATE=desk`). Rules COMP-1 to COMP-10. Not built; all Hirsel type
sketches carry `unbuilt` fences. Archetype C compute placement lives exclusively in this
specification and never inside `fleet@1`
([FLT-1](fleet.md#flt-1-three-workload-archetypes),
[FLT-3](fleet.md#flt-3-control-place-fleet1-and-controller-takeover)).*

Hirsel Compute (`compute@1`) runs Archetype C elastic remote workloads — Tier 1 native units and
contained Wasm tasks, and Tier 2 one-use microVM sandboxes — inside the existing single-binary
`hirsel` launcher and `platform` host. Workers alone admit work against real host resources;
`fleet@1` never tracks compute slots or sits on the create path.

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
  correlated-disruption breaker (`ControlLiveness`, COMP-10).

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
    start_concurrency_slots(class),
    tier_specific_slots(class)
)
```

- **Full-vector atomic debit.** Multiple classes on one host can draw on the same physical bytes,
  address space, file descriptors, connection-table entries, CPU, or disk IOPS. Admission debits
  the full physical resource vector atomically across all dimensions before consuming moved input
  or starting execution; downstream exhaustion paths (unit stack mapping, Wasm `HostMemory`, or
  launcher descriptor exhaustion) become unreachable from compute.
- **Per-create checks at redemption.** Before converting a reservation into occupancy, the worker
  verifies caller authentication, tenant authorization and rights, host cordon state,
  [FLT-6](fleet.md#flt-6-code-trust-revocation-clearance-transport-and-bucket-boundaries) release
  clearance and anti-rollback marks, grant subset, and full resource availability.
- **Lifetime boundaries.** Warm stock, pre-restored paused clones, in-flight starts, active
  occupants, orphans, and retiring children remain charged against the physical ledger until
  their matching termination or reset receipt retires them (`NoOverAdmit`). Start completion
  releases only the start-concurrency charge, never occupancy.
- **Shedding and control protection.** Under overload, a worker stops offering new capacity and
  sheds excess create traffic before expensive decode or allocation while protecting retirement,
  revocation, and control work. Quota refusals are reported distinctly from host saturation
  (`Refused(Quota)` vs. `Refused(Capacity)`).
- *Implementation note (non-normative):* background pre-mapping of native unit stack stock is a
  candidate optimization to keep `mmap`/`munmap` off the Tier 1 create path; if adopted, warm
  stock and mapping-count ceilings enter `capacity[class]` and require separate qualification.

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

### Deployment latency condition

Lane 3087 measured sparse Tier 1 create latency overhead at `+100 us` p50 for `100 us` desk RTT,
crossing the `200 us` added-latency bound at approximately `200 us` effective desk RTT
(dispatcher-to-desk and worker-to-desk round-trip time including desk service delay). Cells
**must** place shard desks within the cell topology such that effective desk RTT stays strictly
below `200 us` for Tier 1 sparse-create workloads.

### Roles and exclusive credit protocol

Three soft-state roles run inside the single `hirsel` binary:

- **Worker.** Owns its physical slots and mints exact, single-use credits (or contiguous ordinal
  windows) only for already-funded capacity within a bounded per-class window `W`. Every credit
  is scoped to the worker's live process `run` (COMP-6) and cannot redeem in any later `run`.
- **Credit desk (shard team).** A soft-state rendezvous on fixed roster-ordered **desk teams**
  named in the cell's signed roster, with each class hashed to one shard/team. In steady state,
  the highest-ranked member of the shard's team is the shard primary and owns the pop transition
  for every connected worker. The desk holds offers and per-dispatcher, per-tenant demand counts
  (`Ask(class, tenant, n)`) — never payloads — and pops each offer to at most one dispatcher
  (`GrantExclusive`), choosing among waiting tenants in proportion to their allocations. *(Team
  size, such as 3 hosts per team, is a non-normative deployment configuration parameter.)*
- **Dispatcher.** Runs in the trusted host process (never inside untrusted tenant code). Parks
  waiting creates within a bounded per-tenant pending cap until a grant arrives or the caller's
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
| **Release** | `SlotBusy(c, generation)` after retirement receipts (COMP-5 / COMP-9) | `SlotBusy -> SlotFree`; remember bounded last outcome per slot |

### Dead dispatchers, partition handoff, and recovery bounds

- **Dead or stalled dispatchers.** The desk records outstanding grants per dispatcher connection
  and caps outstanding grants per connection and per tenant. On disconnect, the desk clears that
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
- **Placement safety invariants (proved in Veil / checked in TLC and Quint, COMP-10):**
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
    tenant_id: u64,
    session_id: u64,
    seq: u64,
    digest_lo: u64,
    digest_hi: u64
}

pub type CreateOutcome: codec + key + send =
    Created(u64)
    | Duplicate(u64)
    | RefusedCapacity
    | RefusedQuota
    | Undecided
```

| Class | Condition | Behavior after an ambiguous send |
|---|---|---|
| **`pure`** | Empty grant subset, deterministic ([SEM-8](semantics.md#sem-8-determinism)), and duplicate-tolerant | May be re-dispatched on a fresh worker exchange within budget; duplicate work is invisible apart from physical occupancy until reclaimed. |
| **`keyed`** | Every reachable effectful sink enforces receiver-checked idempotency on a tenant-scoped logical sub-operation ID and payload digest across workers and retries | May be re-dispatched within budget; every receiver atomically commits deduplication, effect, and replayable outcome, refusing a reused ID with a conflicting digest. |
| **`effectful`** | Everything else, including every Tier 2 sandbox with `Net` and any promised single interactive handle | **Created inert, then activated on the pinned handle.** Never re-dispatched on ambiguity. Pinned recovery queries or replays only the original worker and attempt; if unresolved, returns `Undecided`. |

### Attempt identity and restart fence

Attempt identity is owned by the **caller**, distinct from any placement grant (COMP-3):

- Each logical attempt is bound to `(caller, tenant, non-reusable session, sequence, request
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
    `(host', run')` (where `SEM-8` or the durable receiver's own tenant-scoped idempotency table
    — not the volatile worker — enforces cross-restart deduplication).

### Inert effectful create and pinned activation

An `effectful` create prepares an **inert checkout** on the admitting worker without executing
user code that holds external grants. User code holding grants starts only on an authenticated
activation targeting the returned pinned handle (for Tier 2, `Bind` followed by `Run`, COMP-9). A
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
  invocation as `Undecided`, loses all authority to retry, and may drop its reply to the caller
  (the outward-facing caller recovery contract for `Undecided` is open in
  [Open (owner: Sam)](#open-owner-sam), #1).
- **Total outcomes for a living coordinator (`TotalOutcome`).** Every living, scheduled
  coordinator invocation resolves to exactly one of `Created`, `Duplicate`, `Refused(Capacity)`,
  `Refused(Quota)`, or `Undecided` within budget and by its deadline. Internal placement or
  worker replies (COMP-3 placement outcomes, `PlacementMiss`, `WorkerCapacityRefusal`,
  `Gone(may_have_run)`) are resolved by the dispatcher and never leak raw to callers.
  `Refused(Capacity)` is returned only when every prior worker exchange is known to have had no
  outstanding effect (either a `PlacementMiss` before any worker send, or definitive first-send
  refusals on all prior sends).

## COMP-5: Restart custody (`AccountBeforeMint`) and volatile scope

Compute kernels operate under the volatile scope clause **K11-V** of
[FLT-7](fleet.md#flt-7-proof-ready-protocol-kernel-contract-k1k13): a volatile ledger persists
nothing on the create path and releases directly only because every released execution either
dies with its ledger or is accounted for before its slot is reused.

Because compute children are spawned by the launcher as siblings of the `platform` host process,
a host-process crash can leave compute children running while the volatile admission ledger is
lost. Compute therefore enforces **`AccountBeforeMint`**:

1. **Draw fresh `run` and start closed.** After any host-process restart, the worker draws a
   fresh, non-reusable `run` identifier (COMP-6), starts closed, and advertises or mints **zero**
   capacity. All placement credits and volatile dedup sessions from prior runs are irrevocably
   invalidated by the `run` mismatch (COMP-3, COMP-4).
2. **Fence old session.** At the launcher, fence the previous host session including any queued
   launch or bind commands, close old proxy sessions, and cancel pending proxy dials so no
   in-flight spawn can complete after custody inspection.
3. **Account for every survivor.** Inspect the launcher's **compute custody class** only (never
   Archetype A Place runners or Archetype B serving children). Every surviving compute child must
   be accounted for — either re-adopted into the physical ledger or killed with its descendants
   gone and empty-custody acknowledgement observed from the launcher — before any capacity is
   advertised or any slot is reused. If cleanup cannot be established, the successor remains
   closed. If re-adoption is enabled (subject to [Open (owner: Sam)](#open-owner-sam), #1), any
   re-adopted caller session must also restore its persisted session retirement floor before
   accepting further creates on that session; otherwise pre-restart sessions remain irrevocably
   closed (`LateUnknownId`).

Whether surviving sandboxes on a host-process restart are killed (`ReapBeforeMint`) or re-adopted
through a launcher-held custody note is an open product decision in
[Open (owner: Sam)](#open-owner-sam) (#1). `AccountBeforeMint` holds under either choice.

## COMP-6: Opaque, versioned handles

Every created sandbox or running compute task is addressed by an **opaque, versioned handle**:

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
  (also fencing pre-restart placement credits and attempt sessions, COMP-3–COMP-5, and keying
  usage batches), and `generation` is a non-wrapping per-slot sequence (refusing counter wrap).
  Together with a fresh cryptographic boot key, `(run, slot, generation)` derives a
  domain-separated authenticated secret whose wire width is sized to the fleet-lifetime
  collision/forgery risk budget before freezing the wire format.
- **No `fleet@1` node incarnation.** The durable node incarnation stored in `fleet@1`
  ([FLT-2](fleet.md#flt-2-move-protocol-claimauth-origin-superseded-settlement-deposed),
  [FLT-3](fleet.md#flt-3-control-place-fleet1-and-controller-takeover)) is **not** part of the
  compute handle. Compute fencing never requires a `fleet@1` write on host-process restart.
- **Rights checked on every use (`HandleFresh`).** Routing fields (`cell`, `host`) confer no
  authority. Every operation on a handle (`activate`, `exec`, `signal`, `kill`, `status`)
  authenticates the caller and verifies tenant identity, current rights, live `(run, slot,
  generation)`, and the authenticated secret. Host identifiers are never reused within a fleet:
  a known retired slot, `run`, or host returns `Gone`, while an unreachable owner returns
  `Undecided` (neither settles prior external effects).

## COMP-7: Contained Wasm pricing and slot rules

Tier 1 contained Wasm compute tasks obey
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
  between meters is permitted. Any future CoW or pooled-instantiation path may ship only after
  passing a differential qualification reproducing the CoW-off reference entry's exact results,
  step counts, byte admissions/refunds, failure order, and slice-end positions across element
  segments, extended constants, tight step+byte limits, and slice boundaries.
- **Capacity accounting.** Virtual address space slots, MEM-19 resident bytes, and concurrently
  mounted async stacks are each terms of `capacity[class]` (COMP-2); hosts shard tenant execution
  processes under the single physical host ledger when needed. *(Implementation note,
  non-normative: storing prepared modules in a lock-free once-initialized slot on the module
  record rather than behind a global mutex is a candidate create-path optimization, subject to
  its own qualification.)*

## COMP-8: Multi-cell federation and one-hop spill

A fleet scales horizontally across independent **cells**. Each cell owns its own `fleet@1`
control Place, enrolled roster, placement tier (COMP-3), cleared releases, revocation sequence,
and local `compute_pool`. A cell failure is contained to that cell.

- **Signed manifest of disjoint per-cell allocations.** In the first cut there is **no running
  federation root Place**. Federation state is a root-signed, epoch-versioned **federation
  manifest** listing cells, their SPKI pins, residency tags, and disjoint per-cell tenant
  allocations satisfying `sum_c allocation[t][c] <= entitlement[t]`. Each cell installs the
  manifest through its existing `fleet@1` root-signed keyset and durable anti-rollback path
  ([FLT-6](fleet.md#flt-6-code-trust-revocation-clearance-transport-and-bucket-boundaries)),
  making manifest installation durable and K11-covered per cell. (Whether per-tenant concurrency
  enforcement inside a cell uses hard worker escrow or soft counted limits awaits
  [Open (owner: Sam)](#open-owner-sam), #2.)
- **Decrease before increase.** Shifting allocation from donor cell `X` to recipient cell `Y`
  requires two signed manifests: the first lowers `X` and waits for durable evidence/receipt that
  `X` has relinquished the moved rights; the second raises `Y`. `sum <= entitlement` holds at
  every intermediate state. Rebalancing is an external operator or program tool emitting signed
  manifests.
- **One-hop spill (`SpillDepth <= 1`).** When local placement in the home cell cannot satisfy a
  create, the coordinator may spill **at most once** to one compatible remote cell. The spilled
  request carries a `spilled` mark, and a receiving cell never forwards a marked request again.
- **Summary picks the cell; remote placement decides.** Cells exchange fresh, quantized
  per-cell/class headroom summaries. The home coordinator uses those summaries (filtered by
  residency, tenant entitlement, release clearance, and grant policy) to pick one target cell,
  then sends an exact placement request to that cell's placement tier (COMP-3). A stale summary
  costs at most an exact placement refusal, never a broadcast launch storm.
- **Destination protection.** Receiving cells serve local cell demand before spilled demand and
  enforce inbound per-source, per-class, per-tenant, and aggregate spill caps. Sending cells cap
  outbound spill requests and bytes and suppress spill when fresh remote headroom is absent or
  local observer health is suspect.
- **Cross-cell handle resolution.** Handles carry `(cell, host)` resolved via the manifest and
  target cell roster; no quota crosses a cell boundary at run time.

## COMP-9: Tier 2 full-OS sandboxes

Tier 2 runs on a dedicated KVM-capable host class using the same `compute@1` admission ledger,
handle contract, retry classes, and custody rules as Tier 1:

- **One launcher-supervised VMM per one-use guest.** Each guest runs inside one dedicated
  launcher-supervised VMM process (Firecracker-class) packaged with the single release
  distribution — no fleet-wide VMM daemon, no shell wrapper, and no serving-child-plus-VMM
  process pair. Every checked-out guest is one-use and destroyed on release.
- **No network device in the guest.** Guests have no `virtio-net` device and incur zero
  per-guest TAP, `netns`, `veth`, route, firewall, or RTNL provisioning (qualified by an
  end-to-end host kernel network-configuration trace). Each VMM runs inside a launcher-enforced
  cgroup v2 leaf (memory, pids, CPU, I/O), unprivileged UID, restricted filesystem view,
  allowlisted fd set, no inherited credentials, and a VMM-version-specific seccomp policy.
  *(Implementation note, non-normative: placing all VMM processes into one shared empty network
  namespace created once at host boot with an `AF_UNIX`-only seccomp filter is a candidate
  hardening design, subject to its own lock-trace and launcher qualification.)*
- **File-backed templates and single-tenant frame sharing.** Immutable, signed, content-hashed
  **file-backed** templates (pinning guest kernel, root disk, RAM/device snapshot, VMM version,
  CPU template, and policy) are restored into paused VMMs ahead of demand so process creation and
  snapshot open stay off the create path. Each clone uses private writable disk and RAM mappings.
  Clean template page frames are **shared strictly within a single tenant's trust domain**
  (independent physical backing per tenant, with cross-domain page deduplication excluded from
  qualified hosts).
- **Per-clone uniqueness (`SeedBeforeRun`) and open template-content policy.** Over a
  host-initiated control channel, the worker delivers fresh host entropy, lease identity, and
  wall time (`Bind{seed, lease, wall_time}`) and sends `Run` and tenant credentials only after
  guest acknowledgement (`SeedBeforeRun`). What state a template snapshot is allowed to contain
  (init-only bootstrap, pre-warmed runtimes with reseed hooks, or customer snapshots) and the
  outward-facing Tier 2 isolation contract are **open questions** in
  [Open (owner: Sam)](#open-owner-sam) (#4).
- **Per-tenant vsock proxy child.** Guest egress crosses `virtio-vsock` to a launcher-supervised
  **per-tenant Hirsel child** holding only that tenant's delegated `Net` ceiling. The VMM holds
  zero tenant network authority, and the host admission ledger parses zero guest proxy traffic.
  The proxy authenticates arrival by private host endpoint and live checkout generation (never
  trusting guest-chosen CID, port, or tenant strings) and checks every connect, redirect, and
  resolved address/port against the specific live lease's narrower grant.
- **Host-side handoff and reclaim invariants:**
  - `EgressAfterRun`: the proxy opens no outbound stream for a lease before `Run` is sent.
  - `DialBelongsToLease`: every pending outbound dial carries `(slot, lease generation)` from
    host-owned state; any completion whose generation is no longer live is closed before a byte
    moves.
  - `OneLeasePerVm`: a restored VMM serves at most one checkout.
  - `RevokeBeforeReuse`: a Tier 2 slot (and any tenant quota right, if escrowed under
    [Open (owner: Sam)](#open-owner-sam), #2) is refunded and reusable only after **both**
    receipts arrive:
    1. **Reap receipt** from launcher custody: the VMM and all descendants have exited, the
       cgroup leaf is empty, and writable backing is discarded.
    2. **Broker-close receipt** from the tenant proxy child: all vsock sessions for that lease
       are closed and all pending dials are cancelled.

## COMP-10: Verification and proof split

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
| Worker admission & custody | `NoOverAdmit`, `AccountBeforeMint`, `HandleFresh`, attempt-identity deduplication, `LateUnknownId` retirement floor, and cross-restart `run` fencing (COMP-4, COMP-5) |
| Placement & dispatcher | Placement invariants defined in COMP-3 (`OneLiveCredit`, `SingleUse`, `GrantExclusive`, `WithdrawIsFinal`); dispatcher `TotalOutcome` (living coordinator within 3 worker exchanges and 1 spill), `OutcomeSoundness`, `EffectfulNeverRedispatched` |
| Federation | `SpillDepth <= 1`, `sum_c allocation[t][c] <= entitlement[t]` across every manifest step |
| Tier 2 lifecycle | `SeedBeforeRun`, `EgressAfterRun`, `DialBelongsToLease`, `OneLeasePerVm`, `RevokeBeforeReuse` |
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
  time `<= s`, window `W`, caller deadlines `>= t`, sufficient tenant quota, fair service, no
  failures, and no slot releases within `[0, t]`. *Oracle:* creates started by time `t` are at
  least `min(N, sum_i min(f_i, W * floor(t / (s + r))))`. Includes **zero-slack conservation**:
  `N = sum f_i`, `W = 1`, `deadline = max_i f_i * (s + r)` with zero creates left unstarted.
- **Immediate creates (deterministic oracle).** *Preconditions:* `N` no-deadline creates for one
  eligible class arriving at a reachable placement tier currently holding `q` ready matching-class
  offers, with sufficient tenant quota, fair service, and no failures. *Oracle:* exactly
  `min(N, q)` are granted and the remaining `max(0, N - q)` terminate as `PlacementMiss`
  (`Refused(Capacity)`).
- **Steady-state blocking (Erlang-B statistical oracle).** *Preconditions:* a homogeneous
  single-class pooled loss system with `C` total funded slots, open-loop Poisson arrival rate
  `lambda`, mean occupancy `1 / mu` (offered load `A = lambda / mu` Erlangs), no waiting
  (no-deadline arrivals), sufficient tenant quota, and no failures. *Oracle:* **Erlang-B**
  `B(C, A)` gives the exact blocking lower bound; simulated refusal probability must match
  `B(C, A)` within the stated excess attributable to offer window `W` and in-flight grant RTT,
  with false `Overloaded` tracked separately (COMP-3).
- **Separate fault, fairness, overload, and control-liveness cases.** Asymmetric-partition
  stranded-slot re-offer within the stated recovery bound under COMP-3's liveness assumptions,
  `ControlLiveness` (zero missed A/B heartbeat or `Directory[K]` renewal deadlines under
  saturating compute bursts), tenant max-min fairness, zero spill while home capacity is
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

## Open (owner: Sam)

The following five outward-facing contracts are **open questions** awaiting Sam's ruling. None is
decided by this specification:

1. **Sandbox lifetime across a host-process restart (and caller-visible `Undecided`).** Should
   surviving compute sandboxes be killed on a host-process restart (`ReapBeforeMint`), or should
   they survive and be re-adopted through a launcher-held custody note? What outward-facing
   recovery or ambiguity contract should callers see after an ambiguous create (`Undecided`)?
2. **Hard tenant limits vs. soft limits.** Should tenant concurrency limits be hard escrowed
   rights at workers (zero overshoot, stranded during a host partition, refunded only on reap and
   broker-close receipts), or soft counted limits that can overshoot during partitions and
   rebuilds?
3. **Billing promise.** Should Hirsel promise a hard spend cap (candidate: headroom funded in
   batches through the same escrow) or a best-effort spend cap? What usage loss bound on a
   host-process crash is acceptable (candidate: at most one unflushed batch), and should billing
   be per logical create or per physical execution?
4. **Tier 2 isolation contract.** What outward-facing Tier 2 isolation contract should Hirsel
   promise regarding template contents — candidate: templates snapshotted only from trusted
   initialization (no tenant code or secrets, pre-warmed runtimes only where Hirsel maintains a
   reseed hook, no customer-supplied snapshots) — and how should single-tenant template page-frame
   sharing (decided architecturally in COMP-9) be framed publicly?
5. **Wording of the public "beyond Modal" claim.** What wording and disclosure terms should Sam
   adopt for any public "beyond Modal" claim? (Proposals to choose among or revise: physical
   worker count, distinct useful starts per second over consecutive seconds, simultaneously live
   full-OS guests, published offered/accepted/refused counts, and keeping Tier 1 measurements
   distinct from full-OS Tier 2 claims.)
