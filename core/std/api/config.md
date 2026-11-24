# Config

[API overview](../API.md). [Types and operation contracts](../config.hirsel).

Use `std.config` to read startup configuration from the command line and an
optional [`Env`](env.md) grant. Call `finish` before using the values. A failed
read records the requested key and its problem, so the remaining getters can run
in the same pass.

```hirsel
let mut settings = std.config.new(Some(e), a, 3)
let address = std.config.string_or(mut settings, "APP_ADDRESS", "127.0.0.1")
let port = std.config.port(mut settings, "APP_PORT")
let timeout = std.config.duration_ms_or(mut settings, "APP_TIMEOUT_MS", 5000)
match std.config.finish(take settings) {
    Ok(()) => start(address, port, timeout),
    Error(errors) => report(std.config.format_errors(errors)),
}
```

Pass the number of getter calls to `new` to retain details for every read
that fails. With a smaller bound, the reader keeps the first errors and
counts the rest. `finish` consumes the reader and puts one `More(n)` entry
after the retained errors when the count is positive. It returns `Ok(())`
exactly when no read failed, including when the bound is zero.

## Values and defaults

| Getter | Accepted value | Placeholder on error |
|---|---|---|
| `string` | UTF-8, including an empty value | Empty string |
| `u64` | Unsigned 64-bit decimal integer | `0` |
| `i64` | Signed 64-bit decimal integer, with an optional leading minus | `0` |
| `bool` | Exactly `true` or `false` | `false` |
| `port` | Decimal integer from 1 through 65535, returned as `u16` | `0` |
| `duration_ms` | Unsigned 64-bit decimal millisecond count | `0` |

Numeric input is parsed as written. Whitespace, a leading plus, prefixes,
separators and unit suffixes are invalid. Each `_or` getter returns its
caller-supplied fallback only for an authorized but absent key. Denied,
non-UTF-8 and invalid reads still record an error and return the placeholder.

## Reporting and secrets

`KeyError(key, problem)` distinguishes `Missing`, `Denied`, `NonUtf8` and
`Invalid(String)`. The invalid diagnostic describes the expected format.
`format_errors` joins the key and diagnostic for each error with newlines in
getter call order. It renders `More(n)` as `and N more` and includes no
environment values.

Keys are read directly through `Env`. Keep credentials in `Secret` grants,
which the launcher prevents from overlapping with an `Env` grant.
[Contract tests](../tests/config.hirsel).

## Flags

The reader takes the program's command line as a second source. A flag on the command line
overrides the environment, and both override a getter's fallback. One reader and one
report cover both sources.

```hirsel
fn main(e: Env, a: Args, err: Stderr) -> Result[(), String] waits {
    let mut settings = std.config.new(Some(e), a, 6)
    let port = std.config.port_or(mut settings, "APP_PORT", 8080)
    let verbose = std.config.bool_or(mut settings, "APP_VERBOSE", false)
    let tags = std.config.strings(mut settings, "APP_TAG")
    let level = std.config.log_level_or(mut settings, "APP_LOG", Info)
    let inputs = std.config.positionals(mut settings, 1, 16)
    match std.config.finish(take settings) {
        Ok(()) => run(port, verbose, tags, level, inputs),
        Error(errors) => Error(std.config.format_errors(errors)),
    }
}
```

Run as `app --app-port 9000 --app-verbose --app-tag=a --app-tag b in.csv`.

### Flag names

Every getter key names one flag. The flag is `--` followed by the key with ASCII letters
lowercased and each `_` written `-`: key `APP_PORT` is flag `--app-port`. No other name
reaches the key, so the environment name and the flag name never drift apart, and
`format_errors` names the key the program declared. A key must contain one or more ASCII
letters, digits or underscores. An invalid key gives
`KeyError(key, Invalid("a key is ASCII letters, digits and _"))` when it is read.

### Reading the command line

`new` splits the arguments once, in order. `Args` index 0 is the first program argument
(PROG-3).

| Token | Meaning |
|---|---|
| `--` | Ends the flags. Every later token is a positional, including ones that start with `-`. |
| `--name=value` | Flag `--name` with the value `value`, which may be empty or start with `-`. |
| `--name` | Flag `--name` with no attached value. |
| `-` | A positional. By convention it names standard input. |
| `--help` | A request for usage, unless the program reads key `HELP`. |
| any other token starting with `-` | An unknown flag, including single-dash forms such as `-v` and `-5`. |
| anything else | A *loose* token: a positional, or the value of the flag just before it. |

A flag's value is either attached with `=` or is the loose token directly after it. Which
one depends on the getter that reads the flag, because only the getter knows whether the
flag takes a value:

- **Switches.** `bool` and `bool_or` read a flag as a switch. `--name` alone is `true`.
  `--name=true` and `--name=false` give the value, and any other attached value is
  `Invalid`. A switch never takes the loose token after it, so `--verbose in.csv` leaves
  `in.csv` a positional. `--no-name` is an unknown flag.
- **Valued flags.** Every other getter reads a valued flag. The attached value, or else the
  loose token directly after the flag, is the value. A flag with neither, at the end of the
  command line or followed by another flag or `--`, gives `KeyError(key, NoValue)`. A value
  that starts with `-` must be attached: `--offset=-5`.
- **Claims.** A getter claims the flag tokens it reads and the value token it takes.
  `finish` reports every flag token nothing claimed as `Unknown(token)`, with the text before
  any `=`, and every loose token neither claimed as a value nor taken by `positionals` as
  `Unexpected(token)`. A program that reads no positionals therefore refuses them. No
  argument is silently ignored.
- **Order.** Flags and positionals may be interleaved before `--`. Because a value token is
  never a flag, which tokens are flags is fixed by `new` alone. A loose token becomes a
  value only when the flag before it is read as valued. So the result does not depend on
  the order of the getter calls, except that reading one key both as a switch and as
  valued is a program error that gives `KeyError(key, Invalid("read as a switch and as a
  value"))`.

| Arguments | Getters | Result |
|---|---|---|
| `--b pos --x 1` | `bool(B)`, `u64(X)` | `B = true`, `X = 1`, positional `pos` |
| `--b pos --x 1` | `string(B)`, `u64(X)` | `B = "pos"`, `X = 1`, no positionals |
| `--s --x 1` | `string(S)`, `u64(X)` | `KeyError(S, NoValue)`, `X = 1` |
| `--n -5` | `i64(N)` | `KeyError(N, NoValue)`, `Unknown("-5")` |
| `--n=-5` | `i64(N)` | `N = -5` |
| `--x` at the end | `u64(X)` | `KeyError(X, NoValue)` |
| `--y 3` | `u64(X)` only | `Unknown("--y")`, `Unexpected("3")` |
| `a -- --x` | `positionals(0, 2)`, `u64_or(X, 7)` | `X = 7`, positionals `a`, `--x` |
| `--x 1 --x 2` | `u64(X)` | `KeyError(X, Repeated)` |
| `--tag a --tag=b` | `strings(TAG)` | `["a", "b"]` |

### Sources and precedence

For each read, the flag wins over the environment and the environment wins over the
fallback. A flag given on the command line means the environment variable is not read,
so a denied or non-UTF-8 variable is not an error when the flag supplies the value. Each
source's problems keep their existing meaning. `source` is `None` for a program that
takes no environment grant. Its environment reads then find every key absent, never
`Denied`.

- **Repetition.** A scalar getter whose flag appears more than once gives
  `KeyError(key, Repeated)` and its placeholder. `strings(r, key)` returns every value of
  the flag in command-line order. When the flag is absent, it returns the environment
  value as one element, or an empty list when that is absent too. It never splits a value.
- **Positionals.** Call `positionals(r, min, max)` after the flag getters. It claims the loose
  tokens no flag took, in order. Fewer than `min` gives `KeyError("ARGS", Invalid("expected at least N
  arguments"))`. Tokens beyond `max` stay unclaimed, so `finish` reports them as
  `Unexpected`. A second call returns an empty list.
- **Log levels.** `log_level` reads `debug`, `info`, `warn` or `error`, exactly, as a
  [`trace.Level`](trace.md#logs). Anything else is `Invalid("expected debug, info, warn
  or error")`, and the placeholder is `Info`.

### Help and usage

When the command line holds a bare `--help` before `--` and no getter claims it, every getter still runs, and
`finish` returns `Error([Help(text)])`, with no other errors. `text` is a usage listing built
from the reads, one line per key in call order. Each line shows the flag, ` <value>` for a
valued getter, two spaces, and the environment key. An `_or` getter appends
` (default: VALUE)`. `strings` appends ` (repeatable)`. `positionals` adds a final line,
`ARGS: min..max arguments`. The reader records these lines only when `--help` is present, so a run without
it pays nothing for them. `format_errors` renders `Help(text)` as `text`, `Unknown(t)` as
`t: unknown flag` and `Unexpected(t)` as `t: unexpected argument`. An error names the
token the user typed but never a flag's value or an environment value. Descriptions, grouping, colour and subcommands are not generated. Those belong to a
CLI framework package (API.md, "Keep modules in separate libraries").

### Authority, failures, charging and replay

- **Authority.** `Args` is not a capability (PROG-3): every `main` may take it. The
  environment half still needs the `Env` grant, as before.
- **Failures.** Every problem above is recorded and reported by `finish`. None stops the
  reader. A refused allocation raises `Capacity`, and cleanup refunds the reader (FAIL-5).
- **Charging.** `new` admits one claim byte per argument (`args.len(a)` bytes) under the
  current limit. The reader also admits flag names and their getter modes as each key
  is read, along with the error list's growth. `strings` and `positionals` admit their
  lists, and each value is copied out of `Args`' host memory into an owned `String`. The
  usage lines are admitted only when `--help` is present. Steps: `new` takes
  O(`args.len(a)`). Each getter compares its flag against every flag token, so it takes
  O(total argument bytes) steps, and a program with g getters and A bytes of arguments takes
  O(g × A) steps. With both bounded by the program and its command line, no input chooses
  more work than that.
- **Replay.** No operation passes a replay point. `Args` and the `Env` snapshot enter the
  run with its entry (CAP-8), so a replay reads the same tokens.

### Prior art

Go's `flag` package (`/tmp/ref-go`, go 8eaf19e, `src/flag/flag.go`) is the scope:
long-form flags, `--` as a terminator (`flag.go:1107`), `name=value`, a boolean special case
that takes no next argument (`flag.go:1139`), `flag provided but not defined` refusal
(`flag.go:1136`), and help as an error value (`ErrHelp`, `flag.go:102`).

- **ADOPTED:** switches that never take the next argument, valued flags that take it,
  unknown flags as errors, and `--help` reported as a value, not by exiting.
- **REPLACED:** Go stops at the first non-flag. That needs each flag's arity before the
  scan, which a getter-at-a-time reader learns only when the getter runs. Interleaving, as
  Python's `argparse` (`/tmp/ref-cpython`, cpython 32b51e3, `Lib/argparse.py`) and GNU
  `getopt_long` permute, makes the result independent of getter order. A value that starts
  with `-` must be attached (`--n=-5`), where Go takes `-n -5`. This is what makes the
  flag tokens fixed before any getter runs.
- **REPLACED:** Go lets a repeated scalar flag win last. Here it is `Repeated`, because the
  earlier value would otherwise be silently ignored. `strings` is the repeatable form.
- **NOT TAKEN:** single-dash flags (Go treats `-name` like `--name`), short options and
  bundling (getopt `-abc`), argparse's prefix abbreviation (`allow_abbrev`,
  `argparse.py:2027`), negative switches (`--no-x`), and subcommands.
- **Goose** (`/tmp/ref-goose`, goose d5ae5bb, `stdlib/os.goose:60`) exposes `args()` as raw
  slices and has no flag parser. Its "one function per job" is why flags extend this reader
  rather than adding a second module.

### Open

- **Flag names carry the whole key.** `APP_PORT` is `--app-port`, never `--port`. That
  keeps one name per setting, but CLIs usually drop the application prefix. A
  `new_with_prefix` that strips a declared prefix from flag names only would be additive
  and reversible.
