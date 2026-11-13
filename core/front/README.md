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
  (`-3`, `true`, `0.5`), comma-separated without spaces.

`core/` is a test fixture of the core library: the subset of the spec's canonical
`core/core/` the cases use (prelude, role types, integer operations), as it declares them.
