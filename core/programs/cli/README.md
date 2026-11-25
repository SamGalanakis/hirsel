# cli@1

`cli@1` reads a program's command line and writes its help, errors and shell completion. It is
pure Hirsel over `std@1`: no closures, no derive, no runtime reflection. Its rules follow clap 4
where clap has one, and each departure is listed below.

## Spec

A `Spec` is an arena. `cli.spec(name, about)` starts one, `cli.root(s)` names its root command and
`cli.sub(mut s, at, name, about)` adds a subcommand. Each declarer takes `(mut s, at: Cmd, ...)` and
returns a handle, which is the index of its slot:

| declarer | handle | reads |
|---|---|---|
| `switch`, `negatable` | `Switch` | `--name`, and for `negatable` also `--no-name` |
| `count` | `Count` | `-v` given n times, up to 255 |
| `value` | `Value` | `--name <V>` |
| `integer` | `Int` | `--name <N>`, a signed 64-bit integer |
| `choice` | `Choice` | `--name <V>`, one of a fixed list |
| `many` | `Many` | `--name <V>`, given any number of times |
| `arg` | `Arg` | the next positional word |
| `rest` | `Rest` | every remaining positional word |

`cli.bind_env(mut s, variable)`, `cli.default_to(mut s, text)`, `cli.require(mut s)` and
`cli.global(mut s)` change the flag declared last. A spec that cannot be read unambiguously fails
when it is built: a short name that is a digit (it would read as a negative number) or not an
ASCII letter, `-h`, `-V`, `--help` or `--version` (they are built in), a name declared twice on one
command or under a global flag, a name with a byte other than `A-Z`, `a-z`, `0-9`, `_` and `-`, a
default that is not an integer or not one of the choices, a required positional after an
optional one, or a positional after a rest positional. Long help headings are not offered.

## Parsing

`cli.parse(s, a, source, max_errors)` returns `Run(Matches)`, `Help(Cmd)`, `Version` or
`Failed(Failure)`. Words are read left to right:

- `-vvq` is a cluster; in a cluster a flag that takes a value takes the rest of the word, so
  `-ofile`, `-o=file` (one `=` is dropped) and `-qofile` give `file`, and `-oq` gives `q`.
- `--out=x` and `--out x` give `x`; `--out=` gives the empty value.
- A value starting with `-` must be attached (`--out=-x`), except `-` itself and, for an integer
  flag, a number by clap_lex's `is_number` (`--level -5`). `--out -q` where `-q` is a flag leaves
  `--out` without a value and reads `-q`.
- `--` ends the flags; every later word is positional. A lone `-` is positional.
- Giving a switch or a value twice, or a switch and its negation, is `Repeated`; a count goes on
  counting. A global flag has one value whichever command it is given after.
- A word is a subcommand when it names one before any positional of its command. An unknown
  word where only a subcommand fits is `UnknownCommand`, which stops the scan. Other problems are collected, at most `max_errors`,
  then `More(n)`.
- A flag not on the line takes its environment variable, then its default. An unset, empty or
  denied variable counts as unset; a variable that is not UTF-8 is `Invalid`. A switch reads
  clap's boolish values (`y`, `yes`, `t`, `true`, `on`, `1` and their negatives).
- `-h` and `--help` give `Help` for the command reached; `-V` and `--version` give `Version` on
  the root of a spec with a version.

## Values

`cli.switch_of`, `cli.is_on`, `cli.count_of`, `cli.value_of`, `cli.int_of`, `cli.choice_of`,
`cli.many_of`, `cli.arg_of`, `cli.rest_of` and `cli.command_of` read `Matches`; text comes back as
a view into them. An accessor fails when its handle's slot is out of range or of another kind. A
handle of the same kind from another spec cannot be told apart and reads that slot.
