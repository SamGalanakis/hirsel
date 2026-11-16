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
Observations, including denominators and completion findings, go to the land log
and the ticket event. The committed JSON contains ceilings and policy only.

The driver's `check_cost` example enables this crate's `cost-profile` feature only
through a dev dependency. `check_cost <inputs.json> <new-store> <output.json>
--profile` reports independent nested phase, construct, body and instance views.
It pauses instruction accounting around event bookkeeping; counter wrappers and
changed inlining remain diagnostic overhead. Use the ordinary driver for savings.
Omit `--profile` to export CoreIR, outcomes and definition keys for comparison.
The feature and all scopes are absent from production compiler units.

Land is the only writer of `cost-ceilings.json` and `front-cost-ceilings.json`.
In the merged tree it lowers a cell to its measured margin when that margin is
smaller. New cells start at their observation plus 2%. Source costs round up to
100 instructions per line; front-stage costs round to 100 instructions before
normalizing by tokens. Integer image counts retain their one-event minimum
margin. A rise leaves the ceiling unchanged and is reported against the tickets
in the measured tree. The fixed finished-process budget remains 60,000 per line.
The absolute core-check-index-912 residual allowances apply only to source
comparison cells; the budget and product requests use their full process cost.

Do not edit either ceiling file or record observations in the repository.
An increase needs an explicit reviewed request at
`tools/cost-ceiling-requests/<lane>.json`, for example:

```json
{
  "file": "crates/hirsel-core-front/front-cost-ceilings.json",
  "cell": ["corpora", "m1", "instructions_per_token", "parse"],
  "previous": 100,
  "value": 120,
  "reason": "Explain the newly supported work and the reviewed increase."
}
```

Land checks the requested previous value, applies the increase, records its
reason in the ticket's event, and consumes the request in its generated commit.
The fixed budget cannot be raised through this mechanism. Cost rows are
report-only in land; missing counters and tool crashes still refuse the push.
