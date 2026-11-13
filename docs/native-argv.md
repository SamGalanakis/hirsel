# Native program arguments

A native executable receives only authority supplied at launch. Build a program with `hirsel build app/hirsel.json -o app`, then run `./app --hirsel-grants grants.json -- arg1 arg2`. The grants template must explicitly grant the entry's requested roots.

`hirsel run app/hirsel.json --grants grants.json -- 'foo bar' '' --follow` compiles with Cranelift and supplies three Args values. `--backend llvm` selects LLVM. The delimiter ends compiler options, so flags after it belong to the program.

`hirsel run app/hirsel.json --run run.json` keeps the bound record's Args snapshot and refuses trailing arguments. A grants template can supply an Args snapshot; omitting the delimiter preserves that snapshot, while an explicit empty delimiter replaces it with an empty list.

Wrappers should use `exec` and quoted `"$@"` to preserve spaces, empty values and Unicode. Argument values do not grant filesystem, network or process authority.
