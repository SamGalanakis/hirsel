# Simulation programs

[Operations and contracts](../../sim/exec.hirsel). [Simulation world](world.md).

`serve` runs a one-child model on a `provide.exec` port. The launch selects
the configured program by name. Supply separate ports to model separate
children. Arguments, environment, and directory fields have no effect on
these models.

`Immediate(code)` exits at spawn. `Input(expected)` exits when stdin closes,
returning 0 for exactly the expected input and 1 otherwise. Discarded stdin
starts at EOF. `IgnoreTerm` waits for SIGINT or SIGKILL. A running model accepts
those signals and ends with their signal number. SIGTERM also ends a running
`Input` model.

Each piped output contains a fixed byte sequence available at launch. Reads
respect the caller's room and maximum. Committed reads consume their bytes,
and a drained pipe returns EOF. The model compares input from committed writes
and preserves its state across withdrawn calls.

Pass the same virtual Clock to the server and client, and drive it with
`sim.world.run`. The server's report records its committed signal requests,
the times of SIGTERM, SIGKILL, and the successful terminal wait, and the bytes
read from each output. A withdrawn wait leaves the terminal status available
to a later wait.
