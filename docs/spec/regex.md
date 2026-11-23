# Regex

This chapter specifies std `regex`: regular expressions over `String`. The API uses a lazy DFA with literal prefilters and falls back to the PikeVM.

**Reference.** The pattern language and its meaning are those of the Rust `regex` crate at commit fde4946 (2026-11-18), built with default features off and the features `std`, `perf`, `unicode-case` and `unicode-perl`: its `regex::Regex` type, with the `*_at` semantics of `find_at`, `is_match_at` and `captures_at`. Where this chapter says "the crate", it means that build. A conformance corpus generated from it is the oracle for every answer below. std regex differs from it in one place only, the start-offset precondition of RE-3.

**Naming.** `regex` is std's alone, so another package writes `std.regex.new`, `std.regex.Regex` and so on. Variants are written bare (`Syntax(e)`, `TooBig(n)`).

**Rule numbers.** RE-n are std regex rules.

### RE-1: Values

std regex has four values, and every search operation takes a regex, a cache and a haystack.

```hirsel unbuilt
pub opaque type Regex: shareable + send
pub opaque type Cache: send
pub opaque type Groups: send
pub opaque type Cursor: key
pub type Match: key { start: u64, end: u64 }
pub type SyntaxError: key { at: u64, kind: SyntaxKind }
pub type Error = Syntax(SyntaxError) | TooBig(u64)
```

- A **`Regex`** is a compiled pattern. Nothing changes it after `new`. It is `shareable`, so many tasks can search with one regex, each with its own cache.
- A **`Cache`** holds the mutable search state of one regex: the lazily built automaton states and the scratch of the fallback engine. It is one block, admitted whole when it is made (RE-6). Searching writes into it and never grows it.
- A **`Groups`** holds one span or none for each group of one regex.
- A **`Cursor`** holds the state of an iteration: where the next search starts, whether a match ended there, and whether the iteration is finished (RE-4).
- A **`Match`** is a byte range `[start, end)` of the haystack. `start` and `end` are always scalar boundaries.

Each `Regex` holds an identity from `unique.new()` ([MEM-18](memory.md#mem-18-pools-are-library-code)), made by `new`. `cache(re, ..)` and `groups(re)` record it, and `clone` of a regex, cache or groups keeps it. Two regexes compiled separately from equal patterns have different identities. **Passing a `Cache` or `Groups` made for another regex is a failure** with primary cause `Fail` (RE-7).

```hirsel unbuilt
pub fn new(pattern: String) -> Result[Regex, Error]
pub fn size(re: Regex) -> u64
pub fn cache(re: Regex, budget: u64) -> Cache
pub fn cache_bytes(re: Regex, budget: u64) -> u64
pub fn groups(re: Regex) -> Groups
pub fn group_len(re: Regex) -> u64
pub fn group_index(re: Regex, name: String) -> Option[u64]
pub fn is_match(re: Regex, mut c: Cache, s: String, at: u64) -> bool
pub fn find(re: Regex, mut c: Cache, s: String, at: u64) -> Option[Match]
pub fn captures(re: Regex, mut c: Cache, s: String, at: u64, mut g: Groups) -> bool
pub fn group(g: Groups, i: u64) -> Option[Match]
pub fn cursor(at: u64) -> Cursor
pub fn next(re: Regex, mut c: Cache, s: String, mut k: Cursor) -> Option[Match]
pub fn next_captures(re: Regex, mut c: Cache, s: String, mut k: Cursor, mut g: Groups) -> bool
pub fn count(re: Regex, mut c: Cache, s: String) -> u64
```

std regex has no replace or split operation. Each is a loop over `next` or `next_captures` and `string.push`, and a refused push raises `Capacity` ([MEM-21](budgets.md#mem-21-admission)).

### RE-2: Syntax

`new(p)` accepts exactly the patterns the crate accepts, and refuses every other pattern with the crate's error.
Use `unicode_property_message(p, e)` for an unsupported property's name and the supported properties.

- **Constructs:**
  - literals and escapes;
  - `.` and classes, with ranges, negation, nesting, ASCII classes and the set operations `&&`, `--` and `~~`;
  - the Perl classes `\d`, `\s` and `\w`;
  - repetition (`*`, `+`, `?`, `{n}`, `{n,}`, `{n,m}`) and its lazy forms;
  - capturing, non-capturing and named groups;
  - the flags `i m s U u R x`, set inline or scoped;
  - the assertions `^ $ \A \z \b \B \< \>`, `\b{start}`, `\b{end}`, `\b{start-half}` and `\b{end-half}`.
- **Unicode.** Under the `u` flag, which is on by default, `\d`, `\s` and `\w` are the crate's Unicode tables, and `i` uses the crate's Unicode simple case folding.
  The Unicode word class is the UTS #18 simple word class: Alphabetic, marks (M), decimal digits (Nd), connector punctuation (Pc) and Join_Control. It includes U+0301 COMBINING ACUTE ACCENT and U+2167 ROMAN NUMERAL EIGHT, an alphabetic letter number. `[^\W_]` uses the same class with ASCII underscore removed. So `(?i)k` matches the Kelvin sign, `(?i)[^k]` does not, and `(?i)ss` does not match `ß`. The tables are std constants ([LANG-20](language.md#lang-20-constants)) and are never charged.
- **Unicode properties.** `\p{..}`, `\P{..}` and the one-letter forms are looked up as the crate looks them up. The name is normalized (UAX44-LM3: case, spaces, `_`, `-` and a leading `is` are ignored), then resolved through the crate's property-name and property-value alias tables, with its special cases for `cf`, `sc` and `lc`.
  - A query that resolves to the general category `Decimal_Number` or the binary property `White_Space` is accepted. These are the only properties this build has data for, and they are the `\d` and `\s` tables. So `\p{Nd}`, `\p{digit}`, `\p{gc=Nd}`, `\P{Nd}`, `\p{gc!=Nd}`, `\p{White_Space}` and `\p{space}` are accepted.
  - Every other query is refused with the crate's error: `UnicodePropertyValueNotFound` when the crate cannot resolve a value (`\p{gc=Nope}`, `\p{sc=Nope}`), and `UnicodePropertyNotFound` otherwise (`\pL`, `\pN`, `\p{Greek}`, `\p{sc=Greek}`, `\p{Any}`, `\p{Alphabetic}`, `\p{Nope}`).
- **Always refused**, as the crate refuses them:
  - backreferences and look-around: `UnsupportedBackreference` and `UnsupportedLookAround`;
  - a pattern that could match bytes that are not UTF-8, such as `(?-u:\xFF)` or `(?-u:\W)`: `InvalidUtf8`;
  - a pattern whose syntax tree is nested more than 250 deep, counted as the crate counts it: `NestLimitExceeded`.
- **Which error.** When a pattern has several errors, the error is the crate's. Parse errors come first, in the order the parser meets them. Then the nest limit is checked over the parsed tree. Then translation errors come, in translation order.

A refused pattern returns `Error(Syntax(SyntaxError(at, kind)))`. `at` is the byte offset in `p` where the crate's error span starts, and `kind` is the crate's error kind of the same name. `unicode_property_message(pattern, e)` formats either unsupported-property error with
the query from the original pattern and the two supported properties. It returns
`None` for other syntax kinds or when the pattern has no property at that offset.
It parses the supplied pattern with the usual parser scratch admission.
The kinds are the crate's syntax and translation error kinds, less three this build cannot raise (`UnicodePerlClassNotFound`, `UnicodeCaseUnavailable` and `InvalidLineTerminator`):

```hirsel unbuilt
pub type SyntaxKind: key = CaptureLimitExceeded | ClassEscapeInvalid | ClassRangeInvalid
  | ClassRangeLiteral | ClassUnclosed | DecimalEmpty | DecimalInvalid | EscapeHexEmpty
  | EscapeHexInvalid | EscapeHexInvalidDigit | EscapeUnexpectedEof | EscapeUnrecognized
  | FlagDanglingNegation | FlagDuplicate | FlagRepeatedNegation | FlagUnexpectedEof
  | FlagUnrecognized | GroupNameDuplicate | GroupNameEmpty | GroupNameInvalid
  | GroupNameUnexpectedEof | GroupUnclosed | GroupUnopened | NestLimitExceeded
  | RepetitionCountInvalid | RepetitionCountDecimalEmpty | RepetitionCountUnclosed
  | RepetitionMissing | SpecialWordBoundaryUnclosed | SpecialWordBoundaryUnrecognized
  | SpecialWordOrRepetitionUnexpectedEof | UnicodeClassInvalid | UnsupportedBackreference
  | UnsupportedLookAround | UnicodeNotAllowed | InvalidUtf8 | UnicodePropertyNotFound
  | UnicodePropertyValueNotFound
```

```hirsel
fn compile() -> Result[std.regex.Regex, std.regex.Error] {
  std.regex.new("[a-zA-Z0-9._%+-]+@[a-zA-Z0-9.-]+\\.[a-zA-Z]{2,}")   // Ok
}
fn backreference() -> Result[std.regex.Regex, std.regex.Error] {
  std.regex.new("(a)\\1")                                           // Error(Syntax(SyntaxError(at: 3, kind: UnsupportedBackreference)))
}
fn property() -> Result[std.regex.Regex, std.regex.Error] {
  std.regex.new("\\p{Greek}")                                       // Error(Syntax(SyntaxError(at: 0, kind: UnicodePropertyNotFound)))
}
```

### RE-3: Matching

A search of `s` from `at` gives the crate's `find_at(s, at)` answer: the **leftmost-first** match, which starts earliest at or after `at`, and is the one the pattern prefers among the matches that start there. Alternatives are preferred in order, greedy repetition prefers more and lazy repetition prefers fewer.

- **`at`.** The search starts at `at`, but assertions see the whole haystack: `^`, `\b` and the other look-behind assertions at `at` look at `s` before `at`, as in the crate.
- **The one difference from the crate.** `at` must be at most `string.byte_len(s)` and a scalar boundary, as for `string.slice`. Otherwise the operation fails with `Bounds` (RE-7). The crate accepts an `at` inside a scalar: the empty pattern on `"é"` from 1 gives `[2,2)` there and `Bounds` here.
- **Positions** are byte offsets into `s`. No match, empty or not, starts or ends inside a scalar: a candidate empty match that would is skipped, as in the crate.
- `is_match(re, mut c, s, at)` is whether `find` would return a match.
- `find(re, mut c, s, at)` returns the match, or `None`.

```hirsel
fn leftmost_first(re: std.regex.Regex) -> Option[std.regex.Match] {   // re from new("ab|abc")
  let mut c = std.regex.cache(re, 0)
  std.regex.find(re, mut c, "xabc", 0)                                 // Some(Match(start: 1, end: 3))
}
```

### RE-4: Iteration and counting

A `Cursor` walks the non-overlapping matches of `s` in order, as the crate's `find_iter` does when it is started at `at`.

- `cursor(at)` starts an iteration at `at`. It is not finished, and no match has ended at `at`.
- `next(re, mut c, s, mut k)` searches from the cursor. On a match `[i, j)` it moves the cursor to `j`, records that a match ended at `j`, and returns the match.
- **Empty matches.** A match that is empty and ends where the previous match ended is not returned: the search is repeated from the next scalar boundary, as in the crate's iterator. So `a*` over `"baaab"` returns `[0,0)`, `[1,4)` and `[5,5)`, and never `[4,4)`. When that duplicate is at `string.byte_len(s)`, there is no next boundary: the cursor is finished and `next` returns `None`.
- **The end.** On no match `next` returns `None` and marks the cursor finished. A finished cursor stays finished, and `next` and `next_captures` return `None` and `false` for it without searching.
- **What a cursor holds.** A cursor holds only its position and its two marks, and is not tied to a regex or a haystack. A call may pass a different `s` or `re` than the last one did; it searches that haystack from the held position with the held marks, and fails with `Bounds` when the position is not a valid `at` for it (RE-3).
- `count(re, mut c, s)` is the number of matches `next` returns from `cursor(0)`. It is the crate's `find_iter(s).count()`.

```hirsel
fn errors(re: std.regex.Regex, log: String) -> u64 {   // re from new(" [5][0-9]{2} | [4][0-9]{2} ")
  let mut c = std.regex.cache(re, 1048576)
  std.regex.count(re, mut c, log)
}
```

### RE-5: Captures

A `Groups` holds one span or none for each group of its regex. `groups(re)` makes it with every group none.

- **Groups of a regex.** Groups are numbered as the crate numbers them: group 0 is the whole match, and the capturing groups are numbered from 1 in the order of their opening parentheses. A group that the crate's translation removes, because it lies inside a repetition `{0}` or `{0,0}`, keeps its number, but takes no part in any match and has no name.
  - `group_len(re)` is 1 plus the largest number of a group that is not removed. It is the crate's `captures_len()`. So `(a)|b(?<gone>c){0}` has 2 groups, and `(?<gone>a){0}(?<live>a)` has 3, with group 1 always none.
  - `group_index(re, name)` is the number of the group named `name`, or `None` when no group that is not removed has that name. It agrees with the crate's `capture_names()`.
- `captures(re, mut c, s, at, mut g)` searches as `find` does. On a match it writes every group's span into `g` and returns `true`; the spans are the crate's `captures_at(s, at)` spans in the original haystack, and a group that took no part is none. An empty span (`[i, i)`) that took part differs from none. On no match it returns `false` and sets every group of `g` to none.
- `next_captures(re, mut c, s, mut k, mut g)` is to `captures` what `next` is to `find`. When it returns `false`, every group of `g` is none.
- `group(g, i)` is group `i`'s span, or `None` when it is none or `i` is not below `group_len`.

```hirsel
fn names(re: std.regex.Regex, t: String) -> u64 {   // re from new("\\{\\{(.*?)\\}\\}")
  let mut c = std.regex.cache(re, 0)
  let mut g = std.regex.groups(re)
  let mut k = std.regex.cursor(0)
  let mut total: u64 = 0
  while std.regex.next_captures(re, mut c, t, mut k, mut g) {
    match std.regex.group(g, 1) {
      Some(m) => { total += m.end - m.start },
      None => {}
    }
  }
  total
}
```

### RE-6: Charges

std regex is ordinary std code: it has no bodiless function and adds no core function. Its charges are the [MEM-24](budgets.md#mem-24-logical-charges) charges of its own source. This rule fixes what a caller can rely on.

- **`new(p)`** admits exactly this sequence, with `L = string.byte_len(p)`, and nothing else:
  1. the parse arena, one list of `L + 1` 64-byte nodes, in one admission of `64 × (L + 1)` bytes;
  2. while parsing, the group-name list, a `List[u32]` of two words per named group, grown by `list.push`;
  3. after a successful parse, the translation arena, one list of `L + 1` 64-byte nodes, in one admission of `64 × (L + 1)` bytes;
  4. while translating, the class-range pool, a `List[u32]` of two words per scalar range, grown by `list.push`;
  5. after translation, the parse arena is refunded, and a counting pass sums the program's size without admitting anything. It stops as soon as its running total exceeds `SIZE_LIMIT`; then `new` refunds everything and returns `Error(TooBig(SIZE_LIMIT))` (RE-8);
  6. otherwise the program's lists, each in one admission at its exact size; their total is `size(re)`, a function of `p` alone. Then the translation arena, the range pool and the name list are refunded, and `new` returns `Ok(re)`.

  Growth is charged as [MEM-26](budgets.md#mem-26-growth-charges-the-current-limit) says. Steps 1 to 4 are ordinary admissions under the current limit and have no size limit of their own. When the current limit refuses any admission, the refusal raises `Capacity` ([MEM-21](budgets.md#mem-21-admission)) and the cleanup refunds everything `new` admitted. On `Syntax` everything is refunded too.
- **`cache(re, budget)`** makes one `List[u32]` of exactly `W` words, every word written, and admits it in one admission: `cache_bytes(re, budget)` = `round8(4 × W)` bytes. On refusal it raises `Capacity(requested: cache_bytes(re, budget), available: a)`, where `a` is what the current limit had, and admits nothing. `W` is the layout for `S` automaton states per direction:
  - `W(S) = 2 × S × (C + 20) + F`, with `F = 24 + 3 × (Nf + Nr) + 12 × Nf + 8 × G × (Nf + 1)`;
  - `C` is the number of byte classes of `re` plus one, `Nf` and `Nr` the states of its forward and reverse automata, and `G = group_len(re)`;
  - `S` is the largest whole number with `W(S) ≤ budget / 4` (rounded down), but never below 8 and never above `(2^32 − 1) / C`.

  So `cache_bytes(re, 0)` is the smallest cache, `W(8)`, and the cache also holds the full two-frontier scratch of the fallback engine, for every group. `cache_bytes` is a pure function, so `budget = 2^64 − 1` gives the largest layout, which a real limit refuses.
- **`groups(re)`** makes one `List[u64]` of `2 × group_len(re)` words and admits `round8(16 × group_len(re))` bytes in one admission, or raises `Capacity`.
- **Clones.** Every word of a cache's and a groups' list is written, so a list's length is its capacity, and `clone` admits exactly `cache_bytes(re, budget)` or `round8(16 × group_len(re))` again ([MEM-17](memory.md#mem-17-clone)).
- **Searching admits nothing.** `is_match`, `find`, `captures`, `next`, `next_captures` and `count` call no allocating operation, on any path: clearing, restarting and falling back included. They write only into the `Cache`, `Groups` and `Cursor` they are lent.

**Answers do not depend on the cache.** When the cache is full, it is cleared and keeps the state the search is in. When a state cannot fit even in a cleared cache, or clearing does not pay (RE-7), or a word boundary meets a non-ASCII byte, the search starts again from its own `at` on the fallback engine. So a completed search returns the same answer whatever the cache's size and whatever it held before. Steps are not covered by this: RE-7 says how they depend on the cache.

```hirsel
fn exact(re: std.regex.Regex) -> Result[std.regex.Cache, Capacity] {
  bound bytes std.regex.cache_bytes(re, 0) { std.regex.cache(re, 0) }   // fits; with one byte less, cache raises Capacity
}
```

### RE-7: Steps and failures

std code is user code to [MEM-30](budgets.md#mem-30-step-bounds): every call and loop back edge inside std regex takes a step.

- **Steps depend on the cache.** The steps a search takes follow from std's source, the pattern, the haystack, `at` and the cache's contents. A cache that already holds the states a search needs saves the steps of building them, and a cache size that makes a search clear or fall back adds steps. So one step bound can let a search through with one cache and stop it with another. This is ordinary deterministic state: a cache is a value that only the program's own calls make and change, so the same build, source and replay inputs give the same cache history, the same steps and the same stopping step on every run of that build and in its replay ([SEM-8](semantics.md#sem-8-determinism)); the cache history itself is the same on every build. No engine keeps a cache of its own, and no instruction-set level changes how many loop iterations a skip takes ([SEM-11](semantics.md#sem-11-vectors)).
- **One search.** Let `b` be the bytes a search inspects: forward from `at` until the match is decided (the automaton dies or `s` ends), backward over the match to find its start, and one byte of look-behind. `N = Nf + Nr`, and `G` is `group_len(re)`.
  - Through the automaton, a search takes O(b + 1) steps when every state it reaches is cached, plus O(N) for each state it builds. It builds at most one state per inspected byte.
  - On the fallback engine, it takes O((b + 1) × Nf × (h + 1)) steps. `h` is `G` when the fallback engine must resolve captures and 1 when it needs only the whole match. Each step of a thread copies its `2h` slots. A search restarted on the fallback engine pays its automaton steps before the restart as well.
  - To resolve captures in an automaton match, `captures` and `next_captures` run the PikeVM over the match. This takes O((m + 1) × Nf × (G + 1)) steps for a match of `m` bytes, and none on a miss. The following compile-time test permits fixed offsets instead.
    - The translated HIR must have exactly one participating capture group, numbered 1. The root must be a single sequence consisting of that capture with only literal nodes before and after it. Either literal sequence can be empty. The group is mandatory and has no enclosing alternation or repetition. Its body may contain alternation or repetition. The compiler records the UTF-8 byte widths of the literal prefix and suffix. Patterns with captures outside this shape use the PikeVM.
    - The whole-match search still decides the leftmost-first span `[start, end)`. Every successful path for that span consumes the same literal prefix before entering group 1 and the same literal suffix after leaving it. Thus the group always participates and its span is `[start + prefix, end - suffix)`, regardless of the winning path through the group.
    - Eligible searches pay the ordinary whole-match steps, with `h = 1` if the search falls back. Publishing groups pays its normal call step under MEM-30. Writing the two fixed offsets takes no calls or loop back edges and adds no steps. The evaluator, Cranelift, LLVM and replay charge the same calls and loop back edges.
  - So one search takes O((n + 1) × N × (G + 1)) steps at most, with `n = string.byte_len(s)`.
- **Iteration is not linear.** Each search of an iteration starts where the previous match ended, and may inspect bytes that the previous search already inspected. So `count` and a walk of `next` take the sum of their searches' steps, which is O((n + 1)² × N × (G + 1)) in the worst case. For example, `a+z|a` over `n` bytes of `a` makes `n` searches, each of which reads every byte to its right: Θ(n²) steps. The crate has the same worst case. A pattern whose matches are decided a bounded distance past their end takes linear steps.
- **`new(p)`** takes O(L × 3,900 + k² + size(re)) steps, where `L` is the pattern's byte length and `k` the number of named groups. A class escape materializes at most 817 table ranges, and Unicode case folding scans at most 526 runs and adds at most 3,134 mapped scalars per class. Each name is compared with every earlier one.
- **Under `bound steps`.** A cache declared outside a `bound steps` scope cannot be lent `mut` into it (MEM-30, "Places outside"). To run a search under a bound, make the cache inside the body, or move a prepared cache in and return it with the result.
- **Failures.** The operations fail only with:
  - `Fail`, for a cache or groups of another regex (RE-1);
  - `Bounds`, for an `at` or a cursor position past the end or inside a scalar (RE-3, RE-4);
  - `Capacity`, for a refused admission of `new`, `cache` or `groups` (RE-6), the only operations that admit;
  - `OutOfTime`, at a step under an exhausted bound.
- **Order of checks.** Every search operation checks, in order: the cache's identity, then the groups' identity, then whether the cursor is finished (which ends the call), then `at`. So a cache of another regex is `Fail` even with a bad `at` or a finished cursor.
- **Sites.** `Fail`, `Bounds` and `Capacity` are sited inside the std operation, at the check or the refused core call that raised them: [FAIL-2](failure.md#fail-2-causes) moves sites out of core functions only, and std is not core. `OutOfTime` is sited where MEM-30 puts it. The step of the call into a std regex operation is the caller's call expression, so a bound exhausted there stops at the caller's call, and a bound exhausted inside stops at the call or loop inside std that took it.

```hirsel
fn bounded(re: std.regex.Regex, s: String) -> Result[u64, OutOfTime] {
  bound steps 1000000 {
    let mut fresh = std.regex.cache(re, 0)
    std.regex.count(re, mut fresh, s)
  }
}
```

### RE-8: Size limits

`new` is bounded by one limit on the program it compiles, as the crate is.

- **`SIZE_LIMIT`** is 10,485,760 bytes, the crate's default NFA size limit. `new(p)` returns `Error(TooBig(SIZE_LIMIT))` when the compiled program's size, which `size(re)` would be, exceeds it. Like the crate's `CompiledTooBig(size_limit)`, the error carries the limit, not the size. The counting pass of RE-6 stops once its running total exceeds the limit, before any of the program's lists is admitted, so a `TooBig` pattern admits no program, and a pattern whose size does not fit in `u64`, such as `a{4294967295}{4294967295}`, is `TooBig(10485760)` like any other.
- The limit is on the program only. The parse and translation scratch of RE-6 is not counted toward it: it is admitted under the current limit, and its refusal is `Capacity`, never `TooBig`. So a 90,000-byte literal pattern is accepted, as the crate accepts it.
- The size counts std's own charges, not the crate's memory, so a pattern near the limit may be refused here and accepted by the crate, or the other way round. The conformance corpus has no pattern within a factor of ten of either side's limit, and std's own threshold is checked against hand-derived sizes.
- Searching has no size limit and raises no size error. The cache is bounded (RE-6), and RE-7 bounds a search's steps for every accepted pattern.

```hirsel
fn huge() -> Result[std.regex.Regex, std.regex.Error] {
  std.regex.new("a{1000}{1000}")   // Error(TooBig(10485760))
}
```
