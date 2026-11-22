# Simulation streams and process stop

`capture` and `capture_stderr` create separate virtual streams. Run each
`collect` in its own task. It returns the committed bytes in order once
the run reaches quiescence. Writes withdrawn before reply add no bytes.

`stop_at` starts a task with a virtual clock deadline. At that deadline the
task triggers the process's sticky stop event, as either SIGTERM or SIGINT
does for a host process.
