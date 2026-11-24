# Model

[API overview](../API.md). Verification Phases 1.4 and 1.5. The Bag is implemented.
The build list is
`hirsel-briefs/verify-phase-spec-2034/BUILD-ITEMS.md`.

Version note (2026-11-24, verify-phase-spec-2034): first text. This document specifies
`explore`, `check`, `replay`, `Bag` (Phase 1.4), and `conforms` (Phase 1.5) against
the same `Model` value. It follows the ratified 1.4 plan (BFS over whole states, `always`
plus required counted `sometimes`, `closed` versus `to depth k`), the 2026-11-24
shortlist amendments (VP5, VP8, VP9), and R28 and R29.

`std.model` checks a pure reducer. It explores every state that a finite transition
relation can reach and checks the model's laws on every transition. It is ordinary
Hirsel code: it takes no capability, reads no verdict, and returns ordinary values. As R28
requires, a test runner may observe a result ([law table](../../../docs/laws.md)), but no
program reads a runner's verdict.

**Not in this version.** These parts of the 2026-11-22 plan are deferred to a later
spec pass, which is required before they are built:

- `Env(skew, crash, drop, duplicate, reorder, delay, step_bytes, step_steps)`;
- per-node clocks and `tick`;
- `restart(n)`;
- `std.model.serve`.

The 11-22 text names `skew` and `delay` without defining them. Until that pass, a model
writes faults as its own actions. The [bag example](#example-a-bag-of-messages) delivers
from a `Bag` in any order, and drop and duplicate are actions of the same kind. `grounded`
and every `refines` or `invariant` alias are also out of scope.

## The model

```hirsel
pub type Model[S: key, A] {
    init: List[S],
    start: A,
    actions: own fn(S, mut List[A]),
    step: own fn(S, A) -> S,
    done: own fn(S) -> bool,
    always: List[Always[S, A]],
    sometimes: List[Sometimes[S]],
    show: own fn(A, mut String)
}
pub type Always[S: key, A] { label: String, holds: own fn(S, A, S) -> bool }
pub type Sometimes[S: key] { label: String, holds: own fn(S) -> bool }
pub type Bounds { depth: u64, states: u64, step_steps: u64, step_bytes: u64 }
```

- **`init`** lists the initial states in order. An empty list is refused.
- **`actions(s, out)`** appends every action enabled in `s` to the empty list `out`, in a
  stable order. This list is the complete transition relation: the checker invents no
  action, and an action the list omits is never explored. Action *i* of state *s* is the
  *i*th entry.
- **`step(s, a)`** returns the successor. It is called only with an action that
  `actions(s)` returned.
- **`done(s)`** is true when `s` may legitimately have no successors.
- **`always`** are two-state laws. Each initial state `s` is checked as the pseudo-edge
  `(s, start, s)`, so a law over `next` alone also covers initial states. A law that
  matches on the action sees `start` there.
- **`sometimes`** are reach laws. They are counted on real edges only, never on initial
  states: a state the model starts in is not one that behaviour reached. At least one is
  required.
- **`show`** renders an action in reports.

`Bounds` has no defaults and no constructor function: a law writes all four fields.
`depth` and `states` are count bounds. `step_steps` and `step_bytes` bound each call into
model code. Every call to `actions`, `step`, `done`, a `holds` or `show` runs as
`bound bytes step_bytes { isolate { bound steps step_steps { … } } }`, as a `std.prop` case
does.

State identity is whole-state structural equality (SEM-7). The visited set is a
`Set[S]`. A hash hit is never a match by itself, and there is no fingerprint-only
compaction and no symmetry reduction. A model-only ("ghost") field is an ordinary field
and is part of the key.

## Exploring

```hirsel
pub type Limit = Depth | States
pub type Stats { states: u64, edges: u64, depth: u64, witnessed: List[(String, u64)] }
pub type Outcome = Closed(Stats) | ToDepth(Stats, Limit) | Failed(Failure)
pub opaque type Failure
pub type Kind = Violated(String) | Stuck | Unwitnessed(String) | Handler
pub fn explore[S: key, A](m: Model[S, A], b: Bounds) -> Outcome
pub fn check[S: key, A](m: Model[S, A], b: Bounds) -> Result[(), ()]
pub fn kind(f: Failure) -> Kind
pub fn depth(f: Failure) -> u64
pub fn report(f: Failure, mut out: String)
pub fn token(f: Failure, mut out: String)
```

`explore` is breadth first. Initial states have depth 0. Its order is fixed, so a model
and its bounds always give the same outcome:

1. For each initial state in list order: evaluate every `always` on `(s, start, s)` in
   list order, then insert `s` if it is new. A repeated initial state is not inserted
   twice.
2. Expand the frontier in discovery order. For a state `p` at depth `d < b.depth`, call
   `actions(p)`.
   - **Empty list.** If `done(p)` is false, the outcome is `Failed` with kind `Stuck`
     and the path to `p`. Otherwise `p` is a legitimate terminal state.
   - **Each action** `a`, in list order: compute `n = step(p, a)`, which is one edge,
     counted in `edges`. Evaluate every `always` on `(p, a, n)`, then every `sometimes`
     on `n`, adding 1 to its count when it holds. Only after that, test whether `n` is
     visited. A visited `n` is not inserted again, but its edge has already been
     checked. A new `n` is inserted at depth `d + 1`.
3. The first `always` that returns false ends the walk with `Failed`, kind
   `Violated(label)`, and the path to that edge.

**Results.**

- **`Closed(stats)`.** The frontier emptied, every state was expanded, and every
  `sometimes` count is above 0. If a closed walk leaves a `sometimes` at 0, the outcome
  is `Failed` with kind `Unwitnessed(label)`, for the first such label in list order. The
  whole finite space was explored, so that label holds nowhere.
- **`ToDepth(stats, Depth)`.** A state at depth `b.depth` was discovered and is left
  unexpanded. Expanding stops at `b.depth`, so this is reported even when that state has
  no successors. Its `sometimes` counts may be 0, and `ToDepth` does not judge them.
- **`ToDepth(stats, States)`.** A new state would have been inserted as state number
  `b.states + 1`. Its edge was checked first, so `stats.edges` counts it. The walk stops
  at that edge.
- **`stats.depth`.**
  - Under `Closed`, the greatest depth of a discovered state.
  - Under `ToDepth`, the **complete depth** *k*. Every state reachable in at most *k*
    transitions has been discovered, and every edge out of a state at depth below *k*
    has been checked.
  - Under `Depth`, *k* is `b.depth`. Under `States`, it is the depth of the state being
    expanded when the bound was hit.
- **`Failed` with kind `Handler`.** A call into model code raised a failure. That
  includes `OutOfTime` under `step_steps` and `Capacity` under `step_bytes`. The failure
  carries the path to the state being expanded, and its report names the handler and
  the cause. It is a build's resource outcome when the cause is `OutOfTime` or a bound
  refusal (MEM-30 Q1).

The visited set and the predecessor links are charged to the caller's current limit, as
any `Set` and `List` growth is. A refused admission raises `Capacity` from `explore`. There
is no partial verdict, and `b.states` is the way to stay inside memory with an answer.

`check` returns `Ok(())` on `Closed`. On `Failed` it raises `fail` with the report line,
and on `ToDepth` it raises `model: to depth K (LIMIT), not closed; states S, edges E`. A
law that accepts a horizon matches on `explore` itself.

No outcome is a run stop or a new cause (R29). A violated law, a stuck state, an unwitnessed
`sometimes` and a handler failure are values. `check` turns them into one `fail(text)`.
"Stuck" is not the runtime's `Deadlock` ([FAIL-15](../../../docs/spec/failure.md#fail-15-run-stops)),
which stops real tasks: a stuck model state is a finding about the model.

## Paths, tokens and replay

A failure's path is the BFS predecessor chain. `depth(f)` is the number of transitions
on it:

- for `Violated`, the depth of the edge's source plus 1, or 0 for an initial
  pseudo-edge;
- for `Stuck` and `Handler`, the depth of the state;
- for `Unwitnessed`, 0, because there is no path.

 Because the walk is breadth first, it has
the fewest transitions of any path to the failing edge or state. Its **tape** is the
initial state's index in `init`, followed by the index of each action within its state's
`actions` list.

`token` writes that tape in `std.prop`'s
[token format](prop.md#report-and-replay) with case index 0:
`01, uleb128(0), uleb128(count), uleb128(each value)`, in standard base64 with padding.
The format is unchanged, but the token is decoded by `std.model.replay`, not by `std.prop`.

```hirsel
pub type Replayed = Reproduced(Failure) | NoLongerFails
pub fn replay[S: key, A](m: Model[S, A], b: Bounds, text: String) -> Replayed
```

`replay` follows the tape from its initial state, checking each `always` on each edge and
each state's enabled list as `explore` does. It stops at the first failure on the path
and returns it. If an index is not below its list's length, or the tape ends early,
`replay` raises `model: config: token does not fit this model at step I`.

The report is one line. The token comes first and is never cut:

```text
model: violated: reproduce: model.replay(m, b, "AQAEAAAAAA=="); always never_wrap at depth 3; path: init 0, Inc, Inc, Inc
```

The other kinds have the same layout: `model: stuck: …; at depth D; path: …`,
`model: unwitnessed: sometimes LABEL; closed with S states`, and
`model: handler: …; CAUSE in HANDLER at depth D; path: …`. Rendered paths are cut at
1,024 bytes with `…`.

## Bag

```hirsel
pub opaque type Bag[M: key + ordered]
pub fn bag_new[M: key + ordered]() -> Bag[M]
pub fn bag_add[M: key + ordered](mut b: Bag[M], take m: M)
pub fn bag_remove[M: key + ordered](mut b: Bag[M], m: M) -> bool
pub fn bag_count[M: key + ordered](b: Bag[M], m: M) -> u64
pub fn bag_size[M: key + ordered](b: Bag[M]) -> u64
pub fn bag_entries[M: key + ordered](b: Bag[M]) -> View[List[(M, u64)]] in b
```

A `Bag` is an unordered multiset of messages for a model's network. It is stored as a list of
`(message, count)` pairs, sorted by message with every count at least 1. So two bags that
hold the same messages the same number of times are equal and hash alike (SEM-7), whatever
order they were filled in, and a `Bag` field keeps the state a `key`.

- `bag_add` adds one copy.
- `bag_remove` removes one copy and returns false when there is none.
- `bag_size` counts copies.
- `bag_entries` lists each distinct message once with its count, in message order.

A model enumerates deliveries from `bag_entries`, one action per distinct message, so equal
copies do not multiply the branching.

A bag never collapses copies. Two sends of one message followed by one delivery leave a count
of 1. A model that needs a FIFO link keeps a `List` instead. That restriction is part of
the model, and the bag does not imply it.

## Trace conformance (Phase 1.5)

```hirsel
pub type Boundary[S: key, A, L: key, O: key] {
    label: own fn(A) -> L,
    observe: own fn(S, A, S) -> Option[O],
    show: own fn(L, O, mut String)
}
pub type Seen[L: key, O: key] { hint: L, seen: O }
pub type Reason[L: key] = NoTransition | HintMismatch(L)
pub type Conformance[L: key] = Conforms(u64, u64) | Diverged(u64, u64, Reason[L]) | Incomplete(u64, Limit)
pub fn conforms[S: key, A, L: key, O: key](m: Model[S, A], b: Bounds, v: Boundary[S, A, L, O], trace: List[Seen[L, O]]) -> Conformance[L]
pub fn conforms_check[S: key, A, L: key, O: key](m: Model[S, A], b: Bounds, v: Boundary[S, A, L, O], trace: List[Seen[L, O]]) -> Result[(), ()]
```

`conforms` asks whether a real shell's observed trace is one the model allows, at the
observable boundary only. It takes the same `Model` that `explore` checks, so the model
that conforms is the model that was checked. It does not read the shell's private state.

- A transition `(s, a, n)` is **observable** when `observe(s, a, n)` is `Some(o)`. It is
  **internal** when the result is `None`.
- The shell's trace is the list of what it emitted at its boundary, in order: durable
  writes, replies, sends. Each entry carries the action label the shell claims for it as
  a **hint**.

**The compatible set.** The walk keeps the set of model states the trace could have
reached. **Closing** a set adds every state reachable from it by internal transitions,
which is stuttering. The trace may also end at any point.

1. `C` starts as the closure of `init`.
2. For entry *i* with `(hint h, seen o)`, the next set is the closure of every
   `step(s, a)` with `s` in `C`, `a` in `actions(s)`, `observe(s, a, step(s, a)) == Some(o)`
   and `label(a) == h`. Both the observation and the label must match. A label alone
   never certifies a step, and the observation is checked first.
3. If that set is empty, entry *i* is the end of the **first impossible observable
   prefix**, and the result is `Diverged(i, width, reason)`.
   - `width` is the size of `C` before entry *i*.
   - `reason` is `HintMismatch(l)` when some transition from `C` produces observation
     `o` under another label. `l` is the first such label in exploration order, which
     tells the author that the shell mislabels the step.
   - Otherwise `reason` is `NoTransition`: the model cannot produce `o` from any state the
     prefix allows.
4. If every entry is consumed, the result is `Conforms(entries, widest)`, where `widest`
   is the largest `C` seen.

States are iterated in insertion order and actions in list order, so every result is
deterministic. `always` laws are not re-checked during conformance: `explore` checks the
model, and `conforms` checks the shell against the model. A law about a protocol counts as
grounded in real code only when both pass.

**Bounds.** The bounds are explicit and never hidden.

- `b.states` caps the size of `C`. A closure or step that would grow `C` past it stops
  the walk with `Incomplete(i, States)`.
- `b.depth` caps the internal steps in any one closure. A state that needs more stops the
  walk with `Incomplete(i, Depth)`.
- In `Incomplete(i, _)` and `Diverged(i, _, _)`, *i* counts the entries fully matched
  before the walk stopped. It is also the zero-based index of the entry being
  processed. A cap hit while closing `init` gives `Incomplete(0, _)`.

`Incomplete` is never reported as `Conforms` or `Diverged`, because a capped set may have
dropped the state that matched. Handler calls run under `step_steps` and `step_bytes` as
in `explore`. A handler failure raises its failure out of `conforms`, as the generator
failure does in `std.prop`.

`conforms_check` returns `Ok(())` on `Conforms`. Otherwise it raises one line:

```text
model: conforms: diverged at entry 2 of 3 (recv 2): no transition; 1 compatible state before it
model: conforms: diverged at entry 1 of 2 (send 1): observation fits only under hint recv
model: conforms: incomplete at entry 4: more than 64 compatible states
```

No reverse direction exists in this version. Driving model-chosen actions back into a
shell needs a per-program driver, and none exists yet.

## Example: a counter

```hirsel
type Act = Start | Inc
fn wrap_model() -> std.model.Model[u64, Act] {
    let mut init: List[u64] = list.new()
    list.push(mut init, 0)
    let mut sometimes: List[std.model.Sometimes[u64]] = list.new()
    list.push(mut sometimes, std.model.Sometimes(label: string.from("wrap"), holds: fn(s: u64) -> bool { s == 0 }))
    std.model.Model(
        init: take init, start: Start,
        actions: fn(s: u64, mut out: List[Act]) { list.push(mut out, Inc) },
        step: fn(s: u64, a: Act) -> u64 { (s + 1) % 3 },
        done: fn(s: u64) -> bool { false },
        always: list.new(), sometimes: take sometimes,
        show: fn(a: Act, mut out: String) { string.push(mut out, "Inc") })
}
```

With `Bounds(depth: 10, states: 10, step_steps: 1000, step_bytes: 4096)` this model gives
`Closed(Stats(states: 3, edges: 3, depth: 2, witnessed: [("wrap", 1)]))`, derived by hand:

1. State 0 is initial, at depth 0.
2. Edge 0→1 inserts 1 at depth 1.
3. Edge 1→2 inserts 2 at depth 2.
4. Edge 2→0 is the third edge. `wrap` holds on its `next`, so the count is 1, and 0 is
   already visited.
5. The frontier is empty.

The `wrap` count depends on step 2's order: the edge is checked before the visited
test.

## Example: a bag of messages

The state is `(sent, delivered, net)`, with `net: Bag[u64]`, and starts at `(0, 0, {})`.
`actions` appends `Send` when `sent < 2`, then one `Deliver(m)` for each entry of
`bag_entries(net)`. `Send` adds message 7 and increments `sent`. `Deliver(m)` removes one
`m` and increments `delivered`. `sometimes both: delivered == 2`, and
`done: delivered == 2`.

By hand, the reachable states are written `(sent, delivered)`, and the bag always holds
`sent − delivered` copies of 7. In discovery order:

| depth | state | expanded edges, in action order |
|---|---|---|
| 0 | (0,0) | Send → (1,0) |
| 1 | (1,0) | Send → (2,0), Deliver(7) → (1,1) |
| 2 | (2,0) | Deliver(7) → (2,1) |
| 2 | (1,1) | Send → (2,1), already visited |
| 3 | (2,1) | Deliver(7) → (2,2); `both` +1 |
| 4 | (2,2) | no actions, `done` |

The outcome is `Closed(Stats(states: 6, edges: 6, depth: 4, witnessed: [("both", 1)]))`. A
bag that kept a set would hold one 7 after two sends, never reach `(2,2)`, and report
`Unwitnessed("both")`.

With `done: false`, the same model reports `Failed` with kind `Stuck` at `(2,2)`, depth 4.
Its path is init 0, then action 0 four times (Send, Send, Deliver(7), Deliver(7)). The
tape is `[0, 0, 0, 0, 0]`, so the token is `AQAFAAAAAAA=`.

## Example: conformance

The pipe model has state `(sent, outbox, wire, got)` and starts at `(0, 0, 0, 0)`.

| action | enabled when | effect | observed as |
|---|---|---|---|
| `Send` | `sent < 2` | `sent + 1`, `outbox + 1` | `(Send, sent + 1)` |
| `Ship` | `outbox > 0` | `outbox − 1`, `wire + 1` | internal |
| `Recv` | `wire > 0` | `wire − 1`, `got + 1` | `(Recv, got + 1)` |

The label is the action's name, and the observation `O` is the number seen.

**The trace `[(Send,1), (Send,2), (Recv,1), (Recv,2)]` conforms.** By hand:

| entry | compatible set after closing |
|---|---|
| start | {(0,0,0,0)} |
| (Send,1) | {(1,1,0,0), (1,0,1,0)} |
| (Send,2) | {(2,2,0,0), (2,1,1,0), (2,0,2,0)} |
| (Recv,1) | {(2,1,0,1), (2,0,1,1)} |
| (Recv,2) | {(2,0,0,2)} |

The result is `Conforms(4, 3)`. Without the stuttering closure, the set after
`(Send,1)` would be {(1,1,0,0)} and the set after `(Send,2)` would be {(2,2,0,0)}. Its
wire is empty, so `(Recv,1)` would diverge.

**The trace `[(Send,1), (Recv,1), (Recv,2)]` diverges.**

- After `(Recv,1)` the set is {(1,0,0,1)}.
- No state has `wire > 0`, so `(Recv,2)` is impossible.
- The result is `Diverged(2, 1, NoTransition)`, and the first impossible observable
  prefix is all three entries.

**The trace `[(Send,1), (Send,1)]` diverges on its hint.**

- From {(1,1,0,0), (1,0,1,0)}, a `Send` would be observed as 2.
- `Recv` from `(1,0,1,0)` is observed as 1.
- The result is `Diverged(1, 2, HintMismatch(Recv))`.

## Checker self-test corpus

The corpus lives in `std.model`'s contract tests (`core/std/tests/model.hirsel` once it
exists). It is also 1.4's law set. Each entry is a tiny model with an outcome derived by
hand. Each bad entry names the checker bug that would make it report the wrong outcome.
No model needs more than ten states, so the corpus runs in seconds. There is no
expected-failure registry. A negative entry asserts its exact `Failed` kind, depth and
token in its own body.

| # | Model | Expected | Checker bug it catches |
|---|---|---|---|
| C1 | counter mod 3 (above) | `Closed`, 3 states, 3 edges, depth 2, wrap 1 | a missing edge or state count; `sometimes` evaluated after pruning (wrap 0) |
| C2 | C1 plus `always never_wrap: !(p == 2 && n == 0)` | `Violated("never_wrap")`, depth 3, token `AQAEAAAAAA==` | `always` skipped on an edge into a visited state (reports `Closed`) |
| C3 | C1 with `Bounds.depth = 1` | `ToDepth`, `Depth`, states 2, edges 1, depth 1 | a horizon reported as `Closed` |
| C4 | C1 with `Bounds.states = 2` | `ToDepth`, `States`, states 2, edges 2, depth 1 | a state bound that hides the checked edge, or that reports `Closed` |
| C5 | bag model (above) | `Closed`, 6 states, 6 edges, depth 4, both 1 | a bag that collapses copies (`Unwitnessed("both")`) |
| C6 | bag model, `done` false | `Stuck`, depth 4, token `AQAFAAAAAAA=` | a successor-free state folded into `Closed`; a `done` state reported stuck |
| C7 | two initial states `(0, false)` and `(0, true)`, no actions, `done` true, `sometimes any: true` | `depth 0`: `ToDepth`, `Depth`, states 2, edges 0; `depth 1`: `Unwitnessed("any")` | dedup by hash or by a projection that merges the two states (states 1); `sometimes` counted on initial states (`Closed`) |
| C8 | `init [5]`, `always small: n < 4` | `Violated("small")` at depth 0, token `AQABAA==` | initial states not checked |
| C9 | C1 with `step_steps = 0` | `Handler` at depth 0, `OutOfTime` in `actions` | a handler failure reported as a law verdict, or one escaping as a run stop |

Notes:

- **C7** runs twice. With `Bounds.depth = 0` the outcome is `ToDepth(Stats(states: 2,
  edges: 0, depth: 0, ..), Depth)`. Both states are discovered at depth 0 and left
  unexpanded. With `depth = 1` it is `Unwitnessed("any")`, because `sometimes` counts
  real edges only and the model has none.
- **C8.** The pseudo-edge `(5, start, 5)` fails, so the path is init 0 alone. The tape
  is `[0]`, and the token is base64 of `01 00 01 00`.

Phase 1.5 adds three entries:

| # | Trace | Expected | Bug it catches |
|---|---|---|---|
| T1 | the conforming pipe trace | `Conforms(4, 3)` | no stuttering closure (diverges at entry 2) |
| T2 | the impossible `(Recv,2)` | `Diverged(2, 1, NoTransition)` | a final-state-only check, or a label taken as proof |
| T3 | the mislabeled `(Send,1)` | `Diverged(1, 2, HintMismatch(Recv))` | hints ignored, or hints trusted over observations |
| T4 | the conforming trace with `Bounds.states = 2` | `Incomplete(1, States)`: closing after `(Send,2)` reaches a third state | a cap reported as `Conforms` or `Diverged` |
