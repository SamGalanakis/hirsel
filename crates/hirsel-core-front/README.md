# Core front end

The new language's single check-and-lower traversal. `Program::declare` resolves every
item, then publishes one summary per type declaration (OWN-2: kind, declared bounds,
holds set, layout recipe) in dependency order and checks every signature once. It reads
no body. Inline containment cycles are refused once, at the continuing field of the
member with the least identity; alias chains that reach themselves are refused the same
way (LANG-9). Declared bounds are read, never re-derived (OWN-4).

Constructs this crate does not build yet are recorded as `Gap`s, never accepted silently
and never reported as refusals of the program.

Cases live in `core/front/cases` with hand-derived `// expect:` lines and are checked
against the one core package, `core/core`. See `core/front/README.md`.


The front-end input uses one compilation symbol table and one arena per source file.
`ModuleInput.id` is the index's Copy `ModuleId`. Resolved definitions are `DefinitionId`s;
`Program` maps them directly to declarations/functions, while neutral paths remain the
emission and public query identities. Use and synthesis tables cover only an item's node
range, so many definitions in one file do not allocate file-sized tables per definition.
Generic and parameter names are symbols; the index renders their spelling when needed.

## Cost attribution and regression ceiling

`//:core-check-cost` runs the ordinary release driver on the fixed M1 closure and
core/std, on CPU 16 with one job and a fresh private store. It fails on a missing
counter, an empty corpus, or a check/lower or whole-request cost
above [cost-ceilings.json](cost-ceilings.json). The driver-request ceiling includes
loading, publication and output. A separate whole-process ceiling uses process-tree
`perf stat -e instructions:u`, including dynamic loading before Rust main, reporting
and exit. Both boundaries are printed so startup dependencies remain visible. GAPs stay visible while the checker is
unfinished. This guard records today's work; it does not certify completion.

For local evidence, run `./crates/hirsel-core-front/check-cost.py --compiler <release-hirsel-core>
--request-compiler <release-request-cost> --out <new-directory>`.
Manifest membership stays fixed. The tool counts actual source lines for each run.
The tool reports observations, denominators and completion findings. The
committed JSON contains ceilings and policy.

The fixed product rows divide the whole-process instruction count by the
semantic tokens of the corresponding library sources. `fixed std` uses core
and std, and `fixed core` uses core. The driver excludes trivia and EOF from
both counts. Product marginal rows still subtract the raw fixed std count
and divide by the program's own code lines.

The driver's `check_cost` example enables this crate's `cost-profile` feature only
through a dev dependency. `check_cost <inputs.json> <new-store> <output.json>
--profile` reports independent nested phase, construct, body and instance views.
It pauses instruction accounting around event bookkeeping; counter wrappers and
changed inlining remain diagnostic overhead. Use the ordinary driver for savings.
Omit `--profile` to export CoreIR, outcomes and definition keys for comparison.
The feature and all scopes are absent from production compiler units.

Both cost checks compare measurements against the committed ceiling files.
Proposed ceilings use a 2% margin. Source costs round up to 100 instructions
per line. Front-stage and fixed product costs round up to 100 instructions
before normalizing by tokens. Integer image counts use a minimum margin of
one event. The fixed finished-process budget is 60,000 instructions per line. The absolute
core-check-index-912 residual allowances apply to source comparisons. The
finished budget and product requests use their full process cost.
