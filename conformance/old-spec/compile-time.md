# Compile time

`comptime e` evaluates an ordinary expression while compiling, to initialize a constant or
to produce a value inside a body. Evaluation calls ordinary declared functions, and its
result is materialized data. Static checking still reads only signatures.

```hirsel
fn square(x: u64) -> u64 { x * x }

const TABLE_SIZE: u64 = comptime square(16)

fn scaled(n: u64) -> u64 {
  n * comptime square(4)         // comptime square(n) is CT-2: n is a runtime value
}
```

### CT-1: comptime

`comptime e` evaluates `e` while compiling, to initialize a constant or to produce an
ordinary expression value. Evaluation uses validated ordinary declared calls and
materializes scalar, aggregate, `Map` and `Set` data; dependents check declared signatures.
A failure, an exhausted budget ([CT-4](#ct-4-order)) or a missing capability is a compile error: a well-typed
comptime operation that fails during evaluation is CT-1 at the comptime expression, with the
underlying failure as a note. Static errors inside comptime keep their ordinary owners.
Static checking reads only declared signatures; evaluation executes the selected
implementations and records them as dependencies. There is no implicit build grant, no
reflection over function bodies or source text.

### CT-2: Materialisation

Evaluated values become ordinary constants or expression values before dependents use
them. Only materializable data crosses this boundary: resource, capability and callable
graphs fail CT-2 at the enclosing initializer or explicit comptime expression. An
explicit comptime result must have a data type, checked statically after ordinary
type checking, including in unselected generic definitions. Never and recovery
types do not impose this check; evaluation failures retain CT-1 precedence there.
Evaluation may create and call callable values internally (closures and function
references); only the published result must be data. Publishing callable evaluation results is
reserved. Explicit dictionary declarations follow [GEN-6](generics.md#gen-6-dictionary-constants);
their checked callable references are declared without evaluating callable results.
Implementation contracts ([GEN-6](generics.md#implementation-contracts)) bind operations
by declaration path at check time and evaluate nothing, so this data-only boundary is
unchanged. Named constants materialize dependency first, with ties broken by stable source
path and declaration position, and decode once
into immutable storage that lives for the whole program, before entry. Reads share that
value and construct nothing in a noalloc reading frame; startup runs no initializer code.
Finite evaluated data trees have no materialization depth cutoff.

An inline `comptime` expression admits constants, literals, internal `let`s, ordinary
calls and the enclosing instantiation's size parameters. A size-dependent expression
is staged as a private size-generic constant: its signature is published before
selection, and evaluation occurs for each selected size instantiation. Materialized
results are cached by the constant's size actuals and executed dependencies; changing
an executed implementation, the CT-4 budget or its charging version invalidates the result. Unselected
instantiations do not evaluate. The evaluator, interpreter and native programs use
the same materialized data.

Outer runtime bindings, captures and abstract type names still fail CT-2 at the name,
with the comptime expression as a secondary site. A selected instantiation's failure
keeps CT-1 at the comptime expression and cites the selecting site and size actuals.
Closed expressions inside generic declarations remain legal. Size arithmetic in type
positions remains forbidden.

### CT-4: Order

Compile-time evaluations depend on each other acyclically; a dependency cycle cites its
earliest dependency reference.

Each evaluation (a constant initializer or a selected staged expression) has its own budget B of charged source events, 1,000,000 by default. Charges
are defined on source, before lowering:

- **Calls.** One unit before each executed source call enters its callee: after its
  arguments are evaluated, before transfer or effects. Calls written in
  callbacks count, a `?` error mapper counts, a call to a core primitive costs
  one whatever its implementation, and every tail or constructor-tail restart charges
  its call.
- **Iterations.** One unit on each entered body iteration of a `while` or `for` loop,
  after its condition or `next` succeeds and before body work.
- **Nothing else.** Literals, reads, scalar operators, constructors, branches, matches
  and mechanical cleanup cost zero; source calls and loops they execute still charge.
  Compiler-created calls (iterator scaffolding, forwarding
  wrappers) and optimizer copies add none, while the source bodies they run charge their
  own calls and iterations.

Exactly B units succeed. The next event is denied before it executes and nothing further
runs: CT-1 at the enclosing comptime expression or initializer, with the denied call or
iteration as a secondary site and B as a note. An empty infinite loop therefore stops.
A named constant read does not rerun its initializer, which passed its own budget; a
nested staged evaluation has its own budget and keeps dependency order; no callee raises
its caller's allowance.

```hirsel
fn square(x: u64) -> u64 { x * x }
const N: u64 = comptime square(16)   // one unit: B = 1 succeeds, B = 0 is CT-1

fn count(n: u64) -> u64 {
  let mut i: u64 = 0
  while i < n { i = i + 1 }
  i
}
const C: u64 = comptime count(3)     // four units: the call and three iterations
```

Whether a program compiles is the same at every optimization level, worker count and
cache state: inlining, unrolling, tail restarts and cached results keep the ordered
charges of the source they stand for. Compile-time evaluation runs only on the reference
interpreter; native programs read its materialized results. Evaluated caches are keyed
by the charging version, B and the executed dependencies, so editing the budget
invalidates them and diagnostics retry. Host CPU, memory, call-depth and
dependency-depth safeguards are operational tooling outcomes, not comparable language
results, and PROG-3's interpreter fixture steps are a separate measure.
