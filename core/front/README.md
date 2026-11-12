# Front-end cases

`cases/**/*.hirsel` are the hirsel-core-front corpus: one program per file, module
`app@1/main`, with `// expect:` header lines read by the crate's case harness:

- `// expect: accept`: no refusal and no unbuilt construct.
- `// expect: refuse RULE at LINE:COL`: one line per refusal; the set must match exactly.
- `// expect: kind TYPE KIND`, `// expect: holds TYPE a,b`, `// expect: size TYPE N`: a
  published summary (OWN-2, OWN-3, MEM-24), hand-derived.
- `// expect: run f(ARGS) = RESULTS` or `= fail`: the emitted CoreIR run by the test
  evaluator.

`core/` is a test fixture of the core library: the role types and the few core
signatures the cases call, until the spec's canonical `core/core/` signature files exist.
