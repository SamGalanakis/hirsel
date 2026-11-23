# Simulation world

`std.sim.world.new` creates a virtual clock and a SplitMix64 decision stream.
Start `run` in a task that owns the world. At quiescence it advances the clock
to the earliest pending timer. It returns when quiescence finds no timer.
`step` makes one such advance and returns false when quiescence finds no
timer, for a driver that must also stop on its own signal.

`detach` transfers the world's plan, including its one-shot faults, to a
provider. A plan belongs to one task. Choices within one provider therefore
consume its own stream.

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
