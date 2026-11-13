# Core front end

The new language's single check-and-lower traversal. `Program::declare` resolves every
item, then publishes one summary per type declaration (OWN-2: kind, declared bounds,
holds set, layout recipe) in dependency order and checks every signature once. It reads
no body. Inline containment cycles are refused once, at the continuing field of the
member with the least identity; alias chains that reach themselves are refused the same
way (LANG-9). Declared bounds are read, never re-derived (OWN-4).

Constructs this crate does not build yet are recorded as `Gap`s, never accepted silently
and never reported as refusals of the program.

Cases live in `core/front/cases` with hand-derived `// expect:` lines; `core/front/core`
is a fixture of the core library until the spec's `core/core/` signature files reach the
tools. See `core/front/README.md`.


The front-end input uses one compilation symbol table and one arena per source file.
`ModuleInput.id` is the index's Copy `ModuleId`. Resolved definitions are `DefinitionId`s;
`Program` maps them directly to declarations/functions, while neutral paths remain the
emission and public query identities. Use and synthesis tables cover only an item's node
range, so many definitions in one file do not allocate file-sized tables per definition.
Generic and parameter names are symbols; the index renders their spelling when needed.

## Cost attribution and regression ceiling

`//:core-check-cost` runs the ordinary release driver on the fixed M1 closure and
core/std, on CPU 16 with one job and a fresh private store. It fails on a missing
counter, a changed code-line denominator, or a check/lower or whole-request cost
above [cost-ceilings.json](cost-ceilings.json). The driver-request ceiling includes
loading, publication and output. A separate whole-process ceiling uses process-tree
`perf stat -e instructions:u`, including dynamic loading before Rust main, reporting
and exit. Both boundaries are printed so startup dependencies remain visible. GAPs stay visible while the checker is
unfinished. This guard records today's work; it does not certify completion.

For local evidence, run `./crates/hirsel-core-front/check-cost.py --compiler <release-hirsel-core>
--out <new-directory>`. When the corpus or supported constructs change, measure
once with the same release platform, CPU and jobs, explain the change and update
the recorded denominator and ceiling together. Do not raise the 60,000 whole-request
target to make a regression pass.

The driver's `check_cost` example enables this crate's `cost-profile` feature only
through a dev dependency. `check_cost <inputs.json> <new-store> <output.json>
--profile` reports independent nested phase, construct, body and instance views.
It pauses instruction accounting around event bookkeeping; counter wrappers and
changed inlining remain diagnostic overhead. Use the ordinary driver for savings.
Omit `--profile` to export CoreIR, outcomes and definition keys for comparison.
The feature and all scopes are absent from production compiler units.

Each cost-saving land lowers the observed-work ceilings to its ordinary-driver
sample plus 2%, rounded up to 100 instructions per line. When newly supported
constructs complete more bodies, re-baseline any expected overrun with the
before/after completed-body counts, reason and commit range recorded in
`cost-ceilings.json`. Every lane must explain a red guard in its land message:
new bodies checked, with the count, or a regression to fix. A permanently red
baseline is not useful regression coverage.

The fixed language budget is a separate 60,000 instructions per line for the
finished whole process. The guard prints and checks a completion forecast using
its current process counter plus recorded missing checker/emission and publication
terms. Refresh these terms when support changes: project live construct self
costs over the written source population, calibrate to ordinary checker counters,
and charge completed IR/template bytes per completed definition line for all
unfinished lines at measured encode/write rates. The ceiling file records the
profile revision and completed-body counts. The 50,000 forecast goal of the cost
lane leaves 10,000 headroom below the fixed budget; it does not replace that budget.
Relevant compiler/source lands run the guard and retain both observations and
forecast in their land log. Host-rules permit budget reds to land, but the owning
lane must explain each red or repair the regression.
