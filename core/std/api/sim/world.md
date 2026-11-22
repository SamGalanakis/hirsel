# Simulation world

`std.sim.world.new` creates a virtual clock and a SplitMix64 decision stream.
Start `run` in a task that owns the world. At quiescence it advances the clock
to the earliest pending timer. It returns when quiescence finds no timer.

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
