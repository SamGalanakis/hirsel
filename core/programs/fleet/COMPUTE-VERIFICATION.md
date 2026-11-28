# Compute verification

The [Compute specification](../../../docs/spec/compute.md) defines COMP-1 through COMP-9.

## Deterministic safety obligations

The verification obligations cover one worker with unbounded requests and placement grants.
Each invariant names the rule it follows from:

| Component | Invariant | Statement | Follows from |
|---|---|---|---|
| Worker admission | `NoOverAdmit` | `residual[d] >= 0` for every dimension; every hold, occupancy, and start term is charged until its single return | COMP-2 physical ledger; COMP-3 slot machine |
| Worker admission | `EscrowBalance` | `free[p] + count(charged[p]) + frozen[p] = assigned[p]` at every stable state of a run | COMP-2 spending ledger, donor surrender |
| Worker admission | `NoOverLimit` | live executions of `p` in cell `c` `<=` effective `allocation[p][c]` | `EscrowBalance` and `EscrowConservation` |
| Worker custody | `RetireOnce` | each Redeem charge and each Mint hold is returned exactly once, by the event matching `(host, run, slot, generation, principal)` | COMP-3 retirement table |
| Worker custody | `ActivateOnce` | user code holding grants starts at most once per `(run, slot, generation)` | COMP-3 **Activate**; COMP-4 inert create |
| Worker custody | `AccountBeforeMint` (by `ReapBeforeMint`) | no Mint in a `run` before the empty-custody acknowledgement of that run's own launcher session | COMP-5 |
| Worker custody | `HandleFresh` | the worker accepts a checkout only for the authorized principal and the live credit and slot generation | COMP-6 |
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
| Non-interference | boundary check | compute modules import neither `OutboxCmd` nor `Key` from `types.hirsel` (build-time) | COMP-1 |
| Contained Wasm | reference-entry equivalence | results, step counts, failure order, and replay tapes equal the pinned reference entry with CoW off | COMP-7 |

Timed assume/guarantee liveness obligations are separate from clock-free safety kernels
([FLT-7 K7](../../../docs/spec/fleet.md#flt-7-proof-ready-protocol-kernel-contract-k1k13)), under the COMP-3
liveness assumptions (`delta`, `sigma`, `phi`, `h`):

| Property | Start event | End event | Extra assumptions | Bound |
|---|---|---|---|---|
| `MintLatency` | a slot becomes `SlotFree` (or a class's incremental vector starts to fit) | its `Offer` is ready at the active desk owner | worker not cordoned, overloaded, or closed; fewer than `W` in-flight offers and starts | `sigma + delta + sigma` |
| `StrandBound` | an offer at a desk becomes unredeemable (its desk or the dispatcher it was granted to is lost) | the slot is `SlotFree` again | the worker stays up | `h + sigma` |
| `ReOffer` | competing demand for `W`'s class appears at reachable member `B` while `W`'s offers sit at unreachable `A` | a fresh offer from `W` is ready at `B` | (i)–(vii) of COMP-3 | `phi + h + 2 * (sigma + delta)` |
| `OrphanReclaim` | an `effectful` slot becomes `SlotInert` and its handle is never delivered | the slot's charge and occupancy are returned (**Retire**) | the launcher acknowledges a child's end within `k_end` of its kill | `a + sigma + k_end`, with `a` the activation hold |

## Statistical placement laws (1k, 10k, and 100k logical workers)

Placement quality is checked statistically across paired seeds and open-loop arrivals in one
logical-worker [Fleet placement simulator](placement_sim.hirsel), against exact
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
