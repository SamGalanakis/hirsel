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

## Errors

`cli.report(s, failure)` writes clap's message for a refused command line: each problem on a line
starting `error: `, then the usage of the command reached and `For more information, try
'--help'.`. Missing flags and positionals share one `the following required arguments were not
provided:` list, and `More(n)` reads `n more errors were left out`. An unknown flag, value or
subcommand gets clap's tip: the candidates whose Jaro similarity (strsim 0.11.1) is above 0.7,
the best one for a flag or a value, every one from lowest to highest for a subcommand. An
unknown flag with no suggestion, on a command with positionals, gets the tip to pass it after
`--`. Text from the command line is written through `cli.sanitize`.

## Help

`cli.usage(s, at)` is the line `Usage: git add [OPTIONS] <PATH>...` and `cli.help(s, at,
term_w)` the page of a command for a terminal `term_w` columns wide: its about, its usage, then
`Commands:`, `Arguments:` and `Options:`. Options list the command's own flags, then the global
flags of the commands above it, then `-h, --help` and, on the root of a spec with a version,
`-V, --version`. A flag's help ends with `[env: NAME]`, `[default: x]` and `[possible values:
a, b]`. The help column is two spaces past the widest name in its section; help wraps at the
terminal's width, and a section whose name column takes over 40% of the width while a help would
not fit beside it puts every row's help on the next line, indented 10, as clap does.

## Completion

`cli.bash_completion(s)` is a bash script for `source <(app completion bash)`. It completes a
command's flags after `-`, a choice flag's choices, file names after another value flag, and
otherwise the command's subcommands, or file names when it has none. Bash expands the word list
of `compgen -W`, so `$(...)` in a choice would run when the flag is completed: a choice that is
empty or has a byte other than a letter, a digit or `_-.:+=@%/,` fails when the script is made.
Other shells are not offered.

## Colour

`cli.color(asked, source, terminal)` decides whether to colour a stream. `Always` and `Never`
from `--color` decide alone. With `Auto`, a non-empty `NO_COLOR` turns colour off, then a
non-empty `CLICOLOR_FORCE` other than `0` turns it on, then `CLICOLOR=0` turns it off, then
`TERM` unset or `dumb` turns it off, and otherwise `terminal` decides. An empty or denied
variable counts as unset, except that a denied `TERM` is not consulted. A program grants
`Env=NO_COLOR,CLICOLOR,CLICOLOR_FORCE,TERM` (`cli.COLOR_VARIABLES`) to have them read; without
an `Env` each counts as denied. `cli.paint(text, style, on)` writes one SGR sequence (bold 1,
dim 2, underline 4, colours 30 to 37 and 90 to 97), the text and `ESC[0m`.

## Sanitize

`cli.sanitize(text)` makes every control a terminal acts on visible: U+0000 to U+001F except tab
and newline, and U+0080 to U+009F, in caret notation (`\r` is `^M`; ESC and the C1 CSI U+009B
are both `^[`), U+007F as `^?`, and the bidirectional controls U+202A to U+202E and U+2066 to
U+2069 as `<U+202E>`. Write file names, response bodies and child output through it.

## Width

`cli.width(text)` counts columns: an escape sequence (CSI, OSC or a two-scalar escape) counts 0,
a control or a scalar of category Mn, Me or Cf counts 0, a scalar whose East_Asian_Width is W
or F counts 2 and any other counts 1, Ambiguous included. There is no grapheme segmentation. The
wide table is generated by `core/tools/cli_width_table.py` from a pinned EastAsianWidth.txt, and
a law fails when std's Unicode version moves past it.

## Departures from clap

- A negatable switch shows as `--[no-]color` in help, as git does, instead of clap's two rows.
- There is no `help` subcommand; `-h` and `--help` show the same page, since there is no long
  help.
- `[env: NAME]` does not show the variable's value.
- A row with no help ends at its name, with no padding after it.
- The usage in an error is the command's usage, not one built from the arguments given.
- Help wins over problems found before it on the line.
- A root with positionals and subcommands has one usage line.
- Several problems are reported in one message, one `error:` line each, up to `max_errors`.
