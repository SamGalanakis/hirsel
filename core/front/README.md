# Front-end cases

`cases/**/*.hirsel` are the hirsel-core-front corpus: one program per file, module
`app@1/main`, with `// expect:` header lines read by the crate's case harness:

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
  harnesses compare the kinds.

A run line may end in `evaluator-only:ENGINE,..` (engines `cranelift`, `llvm`): the case
harness holds it under the evaluator, and each named native walker requires it to
disagree. An engine's land that makes it agree removes that engine from the marker and
its NOT-BUILT row in the same commit; the walker fails until it does.

Every case is checked against the one core package, `core/core/`, as every program is.
