# Prop

[API overview](../API.md). The design record is roadmap item 13a.

`std.prop` checks a law against generated values. `search` samples cases and
shrinks a failing case. `all` walks a bounded choice tree. A generator draws its
choices from an explicit choice source. Each sampled case uses a `std.rand.Pcg`
made from the configuration's seed and the case index, so no randomness is global
or ambient.
The engine records every choice on a tape. It shrinks by editing that tape and
running the generator and the law again on the edit. No type has a shrink function.

Shrinking runs in the engine, inside the test's own run, under the test's limits.
`hirsel test` is not changed. A property test that takes no parameters is still
fixed by its program and configuration, so its pass may be reused as any other
test's ([PROG-4](../../../docs/spec/programs.md#prog-4-tests)).

## Generators

```hirsel
pub opaque type Source
pub type Gen[T] { draw: own fn(mut Source) -> T, show: own fn(T, mut String) }
pub fn make[T](take draw_fn: own fn(mut Source) -> T, take show_fn: own fn(T, mut String)) -> Gen[T]
pub fn draw[T](mut s: Source, g: Gen[T]) -> T
```

A `Source` belongs to one run of one case. Only the engine makes one.

A `Gen[T]` holds owning closures, so it is an own value ([LANG-13](../../../docs/spec/language.md#lang-13-callables)), not a plain one.

- Every combinator takes its generators with `take`, and a generator is used once.
- To use the same generator twice, call its constructor twice.
- An owning closure may capture a `Gen`, because a Gen holds an opaque owner, which [LANG-14](../../../docs/spec/language.md#lang-14-captures) allows.
- `just(x)` needs `T` to be plain and inert, since its closure captures `x` and copies it out.

### Choice primitives

These are the only operations that record choices.

- `below(mut s, n) -> u64` records one choice in `[0, n)`. Bounds for `n == 0`.
- `word(mut s) -> u64` records one choice over all of u64. Its bound is written 0 and
  means 2^64.
- `weighted(mut s, k, n) -> bool` records one choice with bound 2. The value 1 is
  true. Fails: Bounds unless `0 < n` and `k <= n`.
- `span(mut s, label: u64, body: scoped fn(mut Source) -> T) -> T` marks the choices
  `body` draws as one span.
- `reject(mut s)` marks the case invalid. Later draws return 0 and record nothing.

How a choice is generated:

- When fresh, `below` draws `std.rand.below(mut pcg, n)`.
- With probability 1/4 it instead draws `std.rand.below(mut pcg, min(n, 16))`. That
  biases a case toward small indices, which are its simplest values. The tier draw is
  `std.rand.below(mut pcg, 4) == 0`, taken first.
- `word` draws `std.rand.next`.
- `weighted` draws `std.rand.below(mut pcg, n) < k`.

The tape stores only the choice, not the Pcg words that produced it. So a change to
how choices are generated never invalidates a printed token.

Replay. When the case replays a tape, draw j reads tape value v_j:

- **Misalignment.** If v_j is not below the bound now asked for, the draw returns 0
  and records 0.
- **Overrun.** A draw past the end of the tape, or past `max_choices`, marks the run
  overrun. The engine discards it.

### Combinators

```hirsel
pub fn bool_() -> Gen[bool]
pub fn u64_in(lo: u64, hi: u64) -> Gen[u64]
pub fn i64_in(lo: i64, hi: i64) -> Gen[i64]
pub fn u8_any() -> Gen[u8]
pub fn char_in(lo: char, hi: char) -> Gen[char]
pub fn ascii() -> Gen[char]
pub fn bytes_of(min: u64, max: u64) -> Gen[Bytes]
pub fn string_of(take chars: Gen[char], min: u64, max: u64) -> Gen[String]
pub fn list_of[T](take g: Gen[T], min: u64, max: u64) -> Gen[List[T]]
pub fn option_of[T](take g: Gen[T]) -> Gen[Option[T]]
pub fn one_of[T](take gs: List[Gen[T]]) -> Gen[T]
pub fn pair[A, B](take a: Gen[A], take b: Gen[B]) -> Gen[(A, B)]
pub fn triple[A, B, C](take a: Gen[A], take b: Gen[B], take c: Gen[C]) -> Gen[(A, B, C)]
pub fn just[T: plain + inert](value: T) -> Gen[T]
pub fn map[A, B](take g: Gen[A], take transform: own fn(A) -> B) -> Gen[B]
pub fn map_shown[A, B](take g: Gen[A], take transform: own fn(A) -> B, take show_fn: own fn(B, mut String)) -> Gen[B]
pub fn bind[A, B](take g: Gen[A], take next_gen: own fn(A) -> Gen[B]) -> Gen[B]
pub fn filter[T](take g: Gen[T], take pred: own fn(T) -> bool) -> Gen[T]
```

Each row gives a combinator's choices and the value of each choice index. Index 0
is always the simplest value.

| Generator | Choices | Value |
|---|---|---|
| `bool_()` | `below(2)` | 0 is false |
| `u64_in(lo, hi)` | `below(hi - lo + 1)`, or `word` when that is 2^64 | `lo + c` |
| `i64_in(lo, hi)` | one choice c over the `hi - lo + 1` values | the zigzag below |
| `u8_any()` | | `u64_in(0, 255)` narrowed |
| `char_in(lo, hi)` | one choice over the scalar values in `[lo, hi]` | in code point order, surrogates skipped |
| `ascii()` | | `char_in(' ', '~')` |
| `list_of(g, min, max)` | span 1 for each element | see below |
| `bytes_of(min, max)` | | `list_of(u8_any(), min, max)` |
| `string_of(chars, min, max)` | | `list_of(chars, min, max)` collected into a `String` |
| `option_of(g)` | `below(2)`, then `g` inside span 2 when it is 1 | 0 is None |
| `one_of(gs)` | `below(len(gs))`, then the chosen generator inside span 3 | |
| `pair(a, b)`, `triple(a, b, c)` | the parts in order, each inside span 4 | |
| `just(x)` | none | `x` (`T` plain and inert) |
| `map(g, f)` | `g`'s choices | `f` of `g`'s value |
| `map_shown(g, f, show)` | as `map` | renders with `show` |
| `bind(g, f)` | `g`'s choices, then `f(value)`'s choices inside span 5 | |
| `filter(g, pred)` | up to three attempts, each inside span 6 | the first value that passes |

Notes on the rows:

- **`i64_in` zigzag.** The origin o is 0 clamped into `[lo, hi]`. Let `m` be the
  smaller of `hi - o` and `o - lo`. While `c <= 2m`, `c = 0` is o, an odd c is
  `o + (c + 1) / 2`, and an even c is `o - c / 2`. Past `2m`, the longer side counts
  on: `o + (c - m)` when `hi - o > o - lo`, and otherwise `o - (c - m)`. So
  `i64_in(-2, 5)` maps indices 0..7 to 0, 1, -1, 2, -2, 3, 4, 5, and `i64_in(3, 9)`
  maps index 0 to 3.
- **`list_of` flags.** Each element after the first `min` starts its span with a
  `weighted(7, 8)` continue flag, and 0 ends the list. At `max` no flag is drawn. So
  `list_of(u64_in(0, 9), 1, 3)` gives [4, 7] for tape [4, 1, 7, 0], and
  [4, 7, 2] for tape [4, 1, 7, 1, 2].
- **`filter`.** When all three attempts fail, `filter` calls `reject`.
- **Span labels.** Labels below 2^32 belong to std.prop. A user generator labels its own spans at 2^32 and above.
- **`map` rendering.** `map` renders the value it mapped from, as `map(<value>)`.

Rendering in counterexample reports uses the consumed choice tape. For a
combinator whose display depends on the draw, call `report` with its
counterexample. Its standalone `Gen.show` prints `<generated>`.

- `bool_` and the integer generators render their literals.
- `char_in` and `ascii` render a quoted char.
- The string, list and tuple generators render `"…"`, `[a, b]` and `(a, b)`.

## Running a property

```hirsel
pub type Config {
    seed: u64, cases: u64, max_choices: u64, max_rejects: u64, max_shrinks: u64,
    gen_steps: u64, case_steps: u64, case_bytes: u64,
    sim_seed: Option[u64], replay: Option[String]
}
pub fn config() -> Config
pub fn replay(take c: Config, text: String) -> Config
pub type Outcome = Passed(u64) | GaveUp(u64, u64) | Falsified(Found)
pub opaque type Found
pub fn search[T](c: Config, g: Gen[T], law: own fn(T) -> Result[(), String]) -> Outcome
pub fn check[T](c: Config, g: Gen[T], law: own fn(T) -> Result[(), String]) -> Result[(), ()]
pub fn report(f: Found, mut out: String)
pub fn token(f: Found, mut out: String)
```

`Passed(cases)` counts valid cases, and `GaveUp(cases, rejected)` also counts
discarded cases.

```hirsel
pub type Case[T: send]: send { value: T, index: u64, world_seed: u64, tasks: Tasks }
pub fn check_tasks[T: send](tasks: Tasks, c: Config, g: Gen[T], law: fn(take Case[T]) -> Result[(), String] waits) -> Result[(), ()] waits
pub fn world_seed(sim: u64, index: u64) -> u64
```

`config()` returns these defaults:

| field | default |
|---|---|
| `seed` | 0 |
| `cases` | 100 |
| `max_choices` | 4096 |
| `max_rejects` | 1000 |
| `max_shrinks` | 1000 |
| `gen_steps` | 1,000,000 |
| `case_steps` | 10,000,000 |
| `case_bytes` | 262,144 |
| `sim_seed` | None |
| `replay` | None |

Case i draws from `std.rand.new(seed, i)`, so any case can be run alone. A first
`word` in case 2 of seed 1 returns 0xc4f5a58656eef510.

How each runner runs a case:

- **`search`.** The engine first generates the value inside `isolate` and
  `bound steps gen_steps`. The tape comes back as a value from a generation that
  succeeds. It then runs
  `bound bytes case_bytes { isolate { bound steps case_steps { law(v) } } }`.
- **`check`.** As `search`, but a `Falsified` or `GaveUp` outcome raises
  `fail(report)`.
- **`check_tasks`.** Each case is spawned with `bytes case_bytes` inside
  `bound steps case_steps` and joined, and the cases run one at a time. A
  `Failed(causes)` from the join is classified the same way as a caught failure.
- **The case's `Tasks`.** `Case.tasks` is the `Tasks` that `check_tasks` was given
  (plain and `send`), so the law can spawn a world's driver and servers.
- **The value in the report.** The case's value moves into the child, so the report
  renders a value the engine generates again from the minimal tape.

In every runner, a refused `bound bytes` entry or spawn fails the test with
`prop: config: case_bytes N refused`. That is never a counterexample.

User code is bounded per run:

- **Steps under `check`.** The user code of a property takes at most
  `(cases + max_rejects + max_shrinks + 1) × (gen_steps + case_steps)` steps.
- **Steps under `check_tasks`.** Each task a case spawns gets its own root bound of
  what is left of `case_steps` ([MEM-30](../../../docs/spec/budgets.md#mem-30-step-bounds)).
  So the bound applies per task, and `case_bytes` limits how many tasks there are.
- **Bytes.** At the defaults, the engine's own bytes stay under 0.5 MiB besides
  `case_bytes`.

Two things these bounds do not cover:

- **Waits.** A law that waits takes no steps while it waits. It ends at
  `--test-timeout` or at a virtual deadline it sets on its world.
- **Stack.** Stack exhaustion is a run stop. It loses the report.

### Exhaustive checking

```hirsel
pub type AllOutcome = Held(u64, u64) | Truncated(u64, u64) | Unbounded(u64) | Falsified(Found)
pub fn all[T](c: Config, g: Gen[T], law: own fn(T) -> Result[(), String]) -> AllOutcome
```

`all` visits choice tapes in lexicographic order, starting with all zeros. After
each leaf it advances the rightmost choice with a remaining value and starts the
suffix at zero. The generator determines how many choices each path needs and
the bound at each position. Different tapes can produce the same value. No
choices come from the PCG.

- `below(n)` visits `0 .. n` and `weighted(k, n)` visits the replay choices
  `{0, 1}`, including when `k` is `0` or `n`.
- `filter` tries one value per path and rejects the path if its predicate fails.
  The law runs only on accepted paths.
- `Held(run, discarded)` means the tree is exhausted. `run` counts paths that
  passed the law, and `discarded` counts rejected paths.
- `Truncated(run, discarded)` reports the same counts for completed paths when
  choices remain at the `cases` limit, a generator attempts a draw past
  `max_choices`, or it exhausts `gen_steps`. `cases` limits attempted paths,
  including rejects.
  Reaching the case limit on the final leaf still returns `Held`.
- `Unbounded(at)` stops the walk at a `word()` draw. `at` is the zero-based
  choice position on that path.
- `Falsified(found)` preserves the first failing tape with zero shrinks. Pass
  its token to `search` with `replay(c, token)` to reproduce that case. Its case
  index counts attempted paths from zero, including rejects.

The law runs under `case_bytes` and `case_steps` as in `search`. A generator
failure other than a traversal or step limit fails as in `search`.
`seed`, `max_rejects` and `max_shrinks` do not affect the walk. `all` refuses a
configuration with a replay token.

### Sim seeds

`Case.world_seed` is `world_seed(sim, index)`, which is `mix(sim ^ mix(index))`.
Here `mix(z)` is the first draw of `std.sim.world.plan(z)` and `sim` is
`sim_seed`, or 0 when it is None.

Vectors:

| call | value |
|---|---|
| `world_seed(0, 0)` | 0xa706dd2f4d197e6f |
| `world_seed(5, 7)` | 0x88bf589a5ce00596 |

The data depends on `seed` alone, and the worlds on `sim_seed` and the case index
alone. While a case shrinks its index stays the same, so its world stays the same.

```hirsel
@test
fn world_matches_plan(e: Env, tasks: Tasks) -> Result[(), ()] waits {
    let mut c = std.prop.config()
    c.sim_seed = std.sim.world.test_seed(e)
    std.prop.check_tasks(tasks, c, std.prop.u64_in(1, 99), fn(take k: std.prop.Case[u64]) -> Result[(), String] {
        let (world, _) = std.sim.world.new(k.world_seed, 0, 0)
        let mut w = take world
        let mut p = std.sim.world.plan(k.world_seed)
        if std.sim.world.choose(mut w, k.value) == std.sim.world.pick(mut p, k.value) {
            Ok(())
        } else {
            Error(string.from("world and plan chose different values"))
        }
    })
}
```

## Shrinking

A failing run has a class:

- `Returned` when the law returned `Error`;
- the primary cause's `CauseKind` tag together with its `Cause.site`;
- `OutOfTime`;
- `Capacity`.

The engine accepts a candidate only when both of these hold:

1. it fails with the class of the current best;
2. the tape it consumed is shorter, or is as long and smaller at its first differing
   value.

The engine runs these passes in order, each scanning in a fixed order:

1. delete spans, largest first, then left to right;
2. delete blocks of 5, 4, 3, 2 and then 1 consecutive choices;
3. set every choice of a span to 0;
4. replace a span with a descendant span that has the same label;
5. minimize each choice: try 0, then 1, then clear high bits, then shift right, then
   subtract multiples of 2 and then of 1;
6. minimize equal choices together;
7. move an amount d from choice i to a later choice j;
8. sort sibling spans that share a label.

The passes repeat until a full round changes nothing or `max_shrinks` candidate runs
are used. No step is random, so a program and its configuration always report the
same counterexample. The search is greedy: its result is a local minimum, and is not
always the least failing tape.

A generator that fails on a search case is reported unshrunk:

```
prop: generator failed at case i
```

During shrinking, a candidate whose generator fails is rejected.

## Report and replay

The report is one line, raised as `fail` from `std.prop`. So the site the runtime
prints is in `core/std/prop.hirsel`, and the test name comes from the runner's label.
The runtime escapes the message's quotes. The text it holds is:

```text
prop: falsified: reproduce: prop.replay(cfg, "AQIDAQoA"); after 3 cases, 41 shrinks: [10]; law: sum >= 10
```

When `sim_seed` is set, `sim seed S; ` comes right after `falsified: `.

The token comes first and is never cut. The rendered value and the law's message
are each cut at 1,024 bytes, with `…`. A token's length is at most
`4 × ceil((21 + 10 × count) / 3)` bytes. Two other
failures use one line each:

- `prop: gave up after N cases, R rejected` when rejections pass `max_rejects`;
- `prop: config: …` for a configuration the engine cannot run.

A token is the standard base64, with padding, of the bytes:

```text
01, uleb128(case index), uleb128(choice count), uleb128(each value)
```

Two worked tokens:

| case | tape | token |
|---|---|---|
| 2 | [1, 10, 0] | `AQIDAQoA` |
| 7 | [3, 0, 300] | `AQcDAwCsAg==` |

When `replay` is set, the engine runs that tape once at that case index. It neither
searches nor shrinks. If the run fails, the engine reports it. If the run passes,
the test fails with `prop: replay token no longer fails`.

A malformed token fails the test with `prop: config: bad replay token`.
In a package that depends on std, write `std.prop.replay(cfg, token)`.

```hirsel
@test
fn sum_stays_small() -> Result[(), ()] {
    std.prop.check(std.prop.config(), std.prop.list_of(std.prop.u64_in(0, 100), 0, 50), fn(xs: List[u64]) -> Result[(), String] {
        let mut total: u64 = 0
        for x in xs { total += x }
        if total < 10 { Ok(()) } else { Error(string.from("sum >= 10")) }
    })
}
```

That property's minimal counterexample is `[10]`, with tape [1, 10, 0].
