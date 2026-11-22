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
