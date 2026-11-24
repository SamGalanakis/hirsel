# Front-end cases

`cases/**/*.hirsel` are the front-end corpus: one program per file, module
`app@1/main`, with `// expect:` header lines read by the driver-level case harness:

- `// expect: accept`: no refusal and no unbuilt construct.
- `// expect: refuse RULE at LINE:COL`: one line per refusal; the set must match exactly.
- `// expect: kind TYPE KIND`, `// expect: holds TYPE a,b`, `// expect: size TYPE N`: a
  published summary (OWN-2, OWN-3, MEM-24), hand-derived.
- `// expect: field-root TYPE FIELD enclosing` or a named region: the field's published root (MEM-6).
- `// expect: outlives FUNCTION A B`: the published reflexive/transitive region relation.
- `// expect: gap WHAT`: one line per kind of construct this implementation does not build
  yet, however many times the case reaches it.
- `// expect: run f(A,B) = R` or `= fail Cause`: every application body is checked and
  emitted; the emitted module must be well formed, and `f` run by the CoreIR evaluator
  must return `R` or fail with `Cause`. Values are written at the signature's types
  (`-3`, `true`, `0.5`), comma-separated without spaces. `= fail Kind@LINE:COL,...`
  also compares each cause's site (SITES.md), resolved in the evaluator; native
  harnesses compare the kinds. A run that returns must have refunded every admission unless the line
  ends in `used N` (after any `trace`), the bytes still admitted when it returns: a
  caught failure keeps its cause cell (FAIL-16), so a run whose `isolate` caught
  one states `used 32`.

The corpus also checks `fixtures/ice-places-358-*.hirsel` with std. The store case
uses its package's manifest. Both malformed programs require diagnostics; the
plain program with its reserved local renamed to `enc` must check.

A run line may end in `evaluator-only:ENGINE,..` (engines `cranelift`, `llvm`): the case
harness holds it under the evaluator, and each named native walker requires it to
disagree. An engine's land that makes it agree removes that engine from the marker and
its NOT-BUILT row in the same commit; the walker fails until it does.

Every case is checked against the one core package, `core/core/`, as every program is.
Use `// library: std` for std fixtures or `// library: runner` to load the runner
and its dependencies from their package manifests.

The standing `hirsel-core-program-tests-unittest` / `cases::corpus` sweep checks
through the driver, with one sealed definition product per body. It walks the
reached instances, joins those units and checks the joined module before evaluation.
Layout assertions inspect the defining materialized unit. The
`call-after-sealed-generic-result.hirsel` vectors retain their scalar arguments,
results and scoped adapters on this same product route.
