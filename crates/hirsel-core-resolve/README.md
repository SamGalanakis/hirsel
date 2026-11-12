# Core resolution

Build a `NameIndex` from declared headers, call `configure_core` with the core package
identity, and resolve each item. The prelude is every public declaration in that
package's `core` module. Type and value lookups have separate namespaces. Core modules
merge visible extension members from the current package and its dependencies, retaining
each member's neutral definition identity. Core-member redeclarations and ambiguous
extension reads are LANG-7 refusals.

Index rows contain names, kind, visibility, fields, variants and declaration sites.
They contain no callee body. Resolution records successful, failed, ambiguous and
shadowed lookups with the namespace and primary site. Bare variants retain expected-type
candidate sets for checking. Dotted paths never name variants. Modes, bounds, anchors
and waits are parsed as written; ownership and semantic checking belong to the next stage.
Resource refusal is an incomplete tool result, separate from source diagnostics.

The test/measurement command accepts an explicit JSON `CorpusManifest`, containing the
core package identity and `PackageManifest` values with source/module mappings and exact
dependencies. It discovers no installed old library and guesses no core declarations.

```sh
.buck2/bin/buck2 run //:hirsel-core-resolve-bin-hirsel-core-resolve -- inputs.json --json results.json
```

Each report refusal includes file, line, column, excerpt and the machine-readable rule
and item-relative site. `--strip-comments` compacts trivia while preserving statement
newlines and literal spellings. `--clean` selects files with no syntax/index/resolve
refusal. `--phase parse|resolve --iterations N` repeats the selected stage. Parsing
includes one corpus header-index build per iteration; resolution reuses one index and
never rebuilds it per definition. Whole-process startup and reporting remain amortized.
