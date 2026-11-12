# Core resolution

Index each module's declarations with `NameIndex::index_item`, then resolve one item
with `resolve`. The index holds names, visibility, field/variant names and declaration
sites; it never holds a callee body. `Environment` supplies exact package exposures and
core-prelude paths. `add_module` also registers empty modules.

Results name neutral `DefPath` identities, item-local lexical binders, module identities,
written projections and expected-type variant candidate sets. `reads` records each
successful, missing, private, opaque or shadowed lookup. Header indexing and lexical
scopes adapt the old resolver's mechanisms; no old compiler crate is imported. Modes,
representations, bounds, uses and waits remain syntax for the checking traversal.

`Resolution::resource_refusal` is an incomplete compiler outcome, not a source diagnostic.
Drivers must not publish incomplete results. Refusals serialize to JSON with one rule,
primary module/item-relative site, causal sites and declared/required facts.

The directory command is a test and measurement entry, not the installed compiler:

```sh
.buck2/bin/buck2 run //:hirsel-core-resolve-bin-hirsel-core-resolve -- core --corpus
.buck2/bin/buck2 run //:hirsel-core-resolve-bin-hirsel-core-resolve -- core/std --signatures
.buck2/bin/buck2 run //:hirsel-core-resolve-bin-hirsel-core-resolve -- core/syntax-examples --library core/std
```

The directory convention uses relative filename components as module names. `--corpus`
reads std signatures as core@1 modules and revision-2 examples as separate example@1
modules, exposing core modules explicitly. `--prelude file.json` supplies an object
mapping names to full neutral paths, such as `{"Capacity":"core@1/Capacity"}`. A mapping
must select a declaration already supplied to the index. Missing core declarations and
outdated seed spellings remain refusals; this command does not invent them.

For an amortized instruction sample, build the binary with
`--target-platforms //tools/buck2:release`, then run it directly under the memory cap
and process-tree perf counter from core/MEASURE.md. `--phase parse --iterations 1000`
repeats lexing, parsing, indexing and destruction; `--phase resolve --iterations 1000`
repeats resolution and destruction over preparsed syntax and a header index. Source
loading, one initial parse/index, line counting, final reporting and process startup
remain in the whole-process count and are amortized. This is not an exclusive-stage
counter or an end-to-end compiler cost. The line denominator counts physical lines
containing a noncomment token, including brace-only lines.

The corpus tests retain byte-identical token-tape round trips and the stronger requirement
that every seed/example parse and resolve without refusals. The latter exposes current
spec/seed disagreements rather than exempting files. Unit tests use independently written
scope, identity, visibility, body-independence, brand and negative-read expectations.
