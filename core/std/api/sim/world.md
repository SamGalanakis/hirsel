# Simulation world

`std.sim.world.new` creates a virtual clock and a SplitMix64 decision stream.
Start `run` in a task that owns the world. At quiescence it advances the clock
to the earliest pending timer. It returns when quiescence finds no timer.

`detach` transfers the world's plan, including its one-shot faults, to a
provider. A plan belongs to one task. Choices within one provider therefore
consume its own stream.
