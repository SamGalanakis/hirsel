# Core resolution

Lex every source with the compilation's `Symbols`, freeze the table in an `Arc`, and
construct `NameIndex::new`. `PackageManifest::intern` and `add_manifests` convert external
package/module descriptions once. Header indexing takes Copy `ModuleId`s and borrowed
file-arena `Item`s. `configure_core` reads the actual public declarations of the core
package's `core` module as the prelude. No core declaration is guessed from a spelling.

Definitions, packages and modules are flat tables addressed by Copy IDs. Names and
members use integer symbols, with separate type and value namespaces. Header rows own
no path or name strings. `definition_path` and `module_path` materialize neutral identities
at driver, checker and output boundaries. Core-front consumes definition IDs directly.

`resolve(index, module, ordinal, item)` returns per-item flat use, binder and dependency
rows. Each written use retains its node and target ID; field suffixes refer to a token
range rather than copied strings. Dependency rows deduplicate scope/symbol/namespace/mode
keys, retaining missing, private, opaque, ambiguous and shadowed outcomes. Local bindings
use one marked undo stack and dense symbol slots. Traversal borrows child ranges and
allocates no child, path or scope objects. Results and scratch tables are reserved before
traversal. Refusal paths alone build diagnostic text.

Locals beat modules, including a dotted path's first component. Local/value-definition
and prelude-function shadowing remains refused. Builtin types resolve first and cannot
be redeclared. Numeric bounds are contextual and legal only on type parameters of bodiless
core declarations. Loop/label targets obey LANG-15 and cannot cross a closure.
Ownership, type fitting, constructor selection and lowering belong to core-front.

The tool accepts a JSON `CorpusManifest`, with explicit source/module mappings and package
dependencies. `--json FILE` writes located refusals and counts; compact module IDs in sites
index the report's `modules` table. `--strip-comments` preserves statement newlines and
literal bytes; `--clean` removes refused definitions and their refused dependents for
measurement input preparation. It retains valid members in partly refused modules.
`--phase parse|resolve --iterations N` repeats one phase, including destruction.
Parse includes corpus header indexing; resolve reuses one index. Startup is amortized.

```sh
.buck2/bin/buck2 run //:hirsel-core-resolve-bin-hirsel-core-resolve -- inputs.json --json results.json
```
