# Env

[API overview](../API.md). [Core signatures](../../core/env.hirsel).
[Host contract](../../../docs/spec/core.md#contracts-the-signatures-do-not-state).

Core module `env` reads the environment variables a run was granted:

```
pub type EnvError = Denied | NotUtf8
pub fn get(e: Env, variable: String) -> Result[Option[View[String]], EnvError] in e
pub fn len(e: Env) -> u64
pub fn name(e: Env, i: u64) -> Option[View[String]] in e
pub fn narrow(e: Env, names: String) -> Result[Env, RuleError]
```

**Authority.** Reading a variable is an explicit authority
([authority is explicit](../../../docs/spec/capabilities.md#cap-1-authority-is-explicit)). An `Env` designates
a list of name patterns. The grant states the patterns, as a Net grant states its rules,
and Deno's `--allow-env=NAMES` works the same way:

```sh
hirsel run app --grant 'Env=HOME,LANG,APP_*'
```

A pattern is an exact name or a prefix followed by `*`. `*` alone covers every name.
Patterns are comma-separated. A name is one or more bytes with no `=`, `,`, `*` or NUL.
Bare `--grant Env` is refused with the required syntax, and full access is written
explicitly as `Env=*`. A run whose Env patterns cover the variable of a `Secret` grant's
`env:` source does not start (exit 103,
[program entry](../../../docs/spec/programs.md#prog-3-entry)). Its line names the variable,
because the `Env` would otherwise give the program the secret's bytes
([secrets](../../../docs/spec/tls.md#tls-5-secrets)).

**Snapshot.** Before `main`, the launcher reads the variables the patterns cover whose
names are valid UTF-8. That snapshot is part of the entry's grants and never changes. The
run has no operation that sets or removes a variable, which is how WASI's `environ_get`
works too. The snapshot lives in host memory outside every limit for the whole run,
like `Args` ([program entry](../../../docs/spec/programs.md#prog-3-entry)).

**Operations.** `get` returns `Ok(Some(v))` for a covered variable that is set,
`Ok(None)` for a covered variable that is not set, and `Error(Denied)` for a name no
pattern covers or that is not a valid name. `Error(NotUtf8)` means the value is not
UTF-8. `len` and `name` list the snapshot's names, sorted bytewise, so the order does not
depend on the host. `name` returns `None` past `len`. `narrow` keeps the part of `e` that
`names` also covers ([capability attenuation](../../../docs/spec/capabilities.md#cap-7-attenuation)). An exact name intersected with a prefix that covers it gives
the exact name. Two prefixes give the longer one when one extends the other. Any other
pair gives nothing, and an empty result is a valid `Env` that covers no name. A malformed
pattern list gives `Invalid` and changes nothing.

**Charging.** `get`, `len` and `name` admit nothing, and their views are anchored in `e`.
`narrow` admits its pattern storage, which stays charged to its payer until the run ends
(`Capacity`), as `net.narrow` does.

**Replay.** `get`, `len` and `name` pass no replay point of their own. The snapshot
enters the run with the entry's arguments
([the replay point](../../../docs/spec/capabilities.md#cap-8-the-replay-point)). When recording is
on, the recording keeps each Env grant's patterns and its snapshot, names and values,
in the grant snapshot. A replay supplies them and does not read the host environment
([recording](../../../docs/recording.md#credentials)). Recording is off by default.
None of these operations waits.
