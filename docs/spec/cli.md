# Command-line interface

Run `hirsel help` for the command catalog. Use `hirsel help <command>` for
the options accepted by a command.

## Native build options

Native binaries from both backends include `.eh_frame` unwind
information. Native builds write detached DWARF alongside the executable.
`--debug-lines on` also embeds DWARF. Failure positions are available in
either mode.
The native unwind records mark where a stack walk ends at a fiber boundary.

## CPU profiles

```text
hirsel prof run [package-dir] [build-opts] [--out FILE.hprof] [--freq 997] [--pmu LIST] [--stack-bytes 32768] [-- args...]
```

`prof run` takes the native build options of `run`. It passes arguments after
`--` to the executable, leaves its standard streams connected, and returns
its exit status. `--out` names the profile and defaults to `profile.hprof`.
The command writes a [portable SQLite profile](profiling.md) after the
process exits. The sampler resolves verified detached DWARF, embedded DWARF
and ELF symbols.
