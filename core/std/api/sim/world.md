# Simulation world

`std.sim.world.new` creates a virtual clock and a SplitMix64 decision stream.
Start `run` in a task that owns the world. At quiescence it advances the clock
to the earliest pending timer. It returns when quiescence finds no timer.
`step` makes one such advance and returns false when quiescence finds no
timer, for a driver that must also stop on its own signal.

`detach` transfers the world's plan, including its one-shot faults, to a
provider. A plan belongs to one task. Choices within one provider therefore
consume its own stream.

## Several clocks

```hirsel
pub opaque type Clocks: send
pub fn clocks(mut w: World) -> Clocks
pub fn drifting(mut c: Clocks, start_mono_ns: u64, start_wall_ms: u64, rate_ppm: i64) -> (u64, Clock)
pub fn step_at(mut c: Clocks, index: u64, at_ns: u64, by_ns: u64)
pub fn pause_at(mut c: Clocks, index: u64, start_ns: u64, end_ns: u64)
pub fn repro(c: Clocks) -> String
pub fn driven(w: World) -> bool
pub fn drive(take c: Clocks, tasks: Tasks) waits
```

For nodes that disagree about time, `clocks` takes the world's clock hand as
clock 0. That clock reads the base timeline exactly. `drifting` adds a clock
whose monotonic reading at base instant `t` (nanoseconds since `clocks`) is
`start + t + t * rate_ppm / 1000000`, truncated toward the base reading. It
returns the clock's index. The rate lies strictly between -1000000 and
1000000, so every clock moves forward. `step_at` jumps a clock forward by
`by_ns` when the base timeline reaches `at_ns`, so its reading from then on
is `by_ns` higher. Wall time stays at its start reading.

`pause_at` freezes one clock during `[start_ns, end_ns)` on the driver's
timeline. That clock resumes without counting the suspended time, including
for drift. A step due during a pause takes effect when the clock resumes.
Pauses on the same clock must be disjoint and nonempty. The driver's
timeline keeps advancing even when clock 0 pauses. `repro` returns the seed
and pauses on one line before `drive` takes the clocks.

Start `drive` in its own task. It is then the only thing that advances any
of these clocks. Each round waits until every task waits. It finds the first
base instant at which some clock reaches its earliest pending timer or a
step falls due, and advances every clock in index order to its reading at
that instant. It returns after two rounds in a row find nothing to advance.
After `clocks`, the world is driven: `run` and `step` fail on it, and
`driven` returns true. `sim.udp.serve` refuses a driven world; start the UDP
provider with [`sim.udp.start`](udp.md#driven-clocks) instead. A world
without `clocks` keeps the single-clock spelling above.

A test that takes an `Env` parameter can pass the seed from `test_seed` to
`new`. `hirsel test --sim --seeds N` runs seeds 0 through N−1 in fresh
processes. `hirsel test --seed S` runs one seed.

## Scripted plans

```hirsel
pub type Script: send { tail_seed: u64, streams: List[List[u64]] }
pub fn script_gen(providers: u64, max_draws: u64) -> prop.Gen[Script]
pub fn plan_from(s: Script, provider: u64) -> Plan
```

Build a `Script` by hand or generate it with `script_gen`. Construct one plan
per provider at setup, using a stable zero-based provider ID. `plan_from`
copies the prefix at that index into the plan. An empty or missing prefix
starts directly at the tail. The tail starts at the SplitMix64 finalizer of
`s.tail_seed ^ provider`, using the finalizer without its state increment.
The first tail draw increments that state by `0x9e3779b97f4a7c15` before
applying the finalizer. Consuming the prefix leaves the tail state untouched.
The order in which providers run has no effect on their individual streams.

For a scripted plan and positive `n`, `pick` consumes one word `w` and returns
`floor(w * n / 2^64)`. This applies to the prefix and the tail. For fixed `n`,
lowering `w` can only lower or preserve the pick, and zero picks index zero.
Plans from `plan(seed)` and `detach` continue to use rejection sampling.

Each plan starts with no faults. Use `script(mut plan, site, fault)` to set a
one-shot fault for a provider's operation site. Draws leave the fault map
untouched. See [property tests with scripted plans](../prop.md#scripted-simulation).

`set_write_faults(mut plan, rates)` sets the chances of `Corrupt`,
`Misdirect` and `LostWrite` on a simulated filesystem. The three chances
are mutually exclusive and measured per 10,000 nonempty writes. Their sum
must be at most 10,000. With all three chances zero, writes consume no draws.
A one-shot fault takes precedence over the distribution at its site.

## Script campaigns

`campaign` accepts a seed-to-`Script` function and a law that consumes a
`ScriptRun`. It tries mutations derived from successful cases. When no mutation
is pending, it calls the seed-to-script function with the campaign seed plus
the case index, wrapping at `u64`. Each case uses the configured child step
and byte limits and derives its world seed from the campaign seed and case
index.

A law calls `pick_observed` on a scripted plan and returns the resulting
`Feedback` with its verdict. Supply stable provider IDs and operation site
numbers. If tasks deliver feedback independently, the law must order it
deterministically before it reaches the campaign. The campaign counts
distinct `(provider, site, outcome bucket)` features up to `max_features`.
It keeps the first successful witness for a new draw location when the
observation adds a feature. It tries replacements at that provider's draw
slot, appending previously consumed words to the prefix when needed. For a
target index `i` at arity `n`, it finds the least word `w` satisfying
`floor(w * n / 2^64) >= i` using wide multiplication. A changed arity can
therefore change a word's outcome.

The first failure returns the concrete `Script`, original case index, and
world seed. `script_tape` encodes a script's words and continuations for
`script_gen` with the same provider count and prefix limit. Use that tape
with [fixed-input task shrinking](../prop.md#scripted-simulation) and retain
the campaign's simulation seed and failure index. The campaign explores only
draws the law observes. Fixed faults and scheduler choices remain part of
the law's own setup.
