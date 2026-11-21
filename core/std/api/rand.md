# Rand

[API overview](../API.md). Specified ahead of its module: the build lane moves each operation
block below into `core/std/rand.hirsel` as that function's `///` contract and cuts this page
down to the module prose.

`std.rand` is a deterministic pseudo-random stream: Go's PCG-DXSM, bit for bit, with Go's
unbiased bounded draw and Fisher-Yates shuffle on top. It has no capability and reads nothing
from outside. The program chooses a seed, and a seed always gives the same stream, on every
target and every release. A stream is for simulation, sampling, test data, load balancing
and jitter. It is **not** for secrets. Anyone who sees enough output or knows the seed can
predict the rest, so keys, tokens and unguessable ids come straight from `host.random`.
Nothing here allocates.

## Types

```
pub opaque type Pcg { hi: u64, lo: u64 }
```

`Pcg` is the 128-bit state of a PCG with Go's multiplier and increment
(`0x2360ed051fc65da4_4385df649fccf645`, `0x5851f42d4c957f2d_14057b7ef767814f`) and the DXSM
output function. Every state is valid. Its fields are plain, so the type is plain
([OWN-3](../../../docs/spec/ownership.md#own-3-kinds)): `let fork = g` forks a stream
deliberately. Draws take it `mut`, so a stream does not repeat by accident.

## Seeding

There are two seeds, and the choice between them is the caller's.

- **Replayable:** `std.rand.new(info.seed, 0)` from a store call's `CallInfo.seed`. Each call
  gets a different stream, and a replay of the call gets the same one. Any other `u64` the
  program derives (a test's constant, a hash of a job id) works the same way.
- **Fresh:** 16 bytes from `host.random(r, mut b)`, then
  `std.rand.new(list.word_le(b, 0), list.word_le(b, 8))`. The bytes are a delivery that
  passes the replay point ([CAP-8](../../../docs/spec/capabilities.md#cap-8-the-replay-point)),
  so a run given the same bytes draws the same stream. It is
  still not a secret source; it is only unpredictable at the start.

## Operations

`new(seed1: u64, seed2: u64) -> Pcg`
: The state `hi = seed1, lo = seed2`, as Go `NewPCG(seed1, seed2)` does. There is no mixing
  step, so Go's published outputs for a seed pair are this module's outputs too. Cost: O(1).

`state(p: Pcg) -> (u64, u64)`
: `(hi, lo)`. `new` of that pair continues the stream from the same point. This is how a
  stream is saved in a record and resumed (Go `MarshalBinary` carries the same two words).

`next(mut p: Pcg) -> u64`
: Advances `state = state * mul + inc (mod 2^128)` and returns DXSM of the new state:
  `h = hi; h ^= h >> 32; h *= 0xda942042e4dd58b5; h ^= h >> 48; h *= (lo | 1)`, with wrapping
  multiplies (Go `pcg.go`, `next` and `Uint64`). The 128-bit product uses `int.mul_wide`.
  Cost: O(1), three 64-bit multiplies.

`below(mut p: Pcg, n: u64) -> u64`
: Uniform in `[0, n)`, with no bias. This is Go `Rand.uint64n` on a 64-bit target. A power of
  two masks one draw: `next & (n-1)`. Otherwise `(hi, lo) = mul_wide(next, n)`. When
  `lo < n` it computes `t = (0 - n) % n` and draws again while `lo < t`. The result is `hi`.
  The number of draws consumed is part of the contract, because later outputs depend on it.
  Fails: Bounds (failure) when `n == 0`. Cost: expected fewer than 2 draws for every `n`.
  Ex: `let die = std.rand.below(mut g, 6) + 1`

`float64(mut p: Pcg) -> f64`
: Uniform in `[0, 1)`: the low 53 bits of one draw, times 2^-53 (Go `Float64`,
  `float64(x<<11>>11) / (1<<53)`). Every multiple of 2^-53 below 1 is equally likely, and 1 is
  never returned. Cost: one draw.

`shuffle[T](mut p: Pcg, mut xs: List[T])`
: Fisher-Yates from the end: for `i` from `len-1` down to 1, `list.swap(xs, i, below(p, i+1))`
  (Go `Shuffle`). Each of the `len!` orders is equally likely as far as the 128-bit state
  allows. Cost: `len - 1` bounded draws. Ex: `std.rand.shuffle(mut g, mut deck)`

A signed range `[lo, lo + size)` is `lo + int.truncate(below(g, size))`, for a `size` that fits `i64`. A choice from a list is
`list.get(xs, below(g, list.len(xs)))`. A Bernoulli trial with probability `k/n` is
`below(g, n) < k`. These are one line each and get no functions of their own.

## Decisions

- **Go's PCG-DXSM, ADOPTED unchanged** (go 46b62495 `src/math/rand/v2/pcg.go`). It is
  small, fast, passes BigCrush and PractRand, has a stream that is fixed and published, and
  its constants trace to O'Neill's `pcg_random.hpp`. Goose's `rng` is splitmix64
  (Goose `stdlib/std.goose:271-281`). Splitmix has one word of state and is simpler, but it
  has no published vector set as broad as Go's `regressGolden`, and a 64-bit state limits the
  orders `shuffle` can reach. Splitmix is NOT TAKEN for the stream. The store already uses it
  to derive `CallInfo.seed`, which is a different job.
- **Bounded draws: Go's Lemire multiply-and-reject, ADOPTED** (`rand.go:84-120`). Goose's
  `rand_int` (`std.goose:287`) uses modulo with the same rejection threshold. It is unbiased
  too, but it costs a division on every draw. The two consume draws differently, so the
  choice is fixed here, and it follows Go so that Go's tables are the oracle. Go's 32-bit
  fast path (`is32bit`) is NOT TAKEN. It changes the stream on 32-bit targets, and one seed
  must give one stream everywhere.
- **`float64` takes the low 53 bits**, as Go does. Goose takes the top 53. Both are uniform,
  but the outputs differ. Go's choice is ADOPTED because `regressGolden` pins it.
- **The stream is part of the contract.** A seed's outputs never change. A better generator
  would be a new type next to `Pcg`, not a change to it. Rust's `rand::StdRng` deliberately
  reserves the right to change algorithm between releases, which breaks recorded
  replays; that is NOT TAKEN.
- **No global or thread generator.** Go's top-level `rand.IntN` reads a hidden
  runtime-seeded source. Here every stream is a value the program holds (REPLACED).

## Left out

Signed and float ranges beyond the one-liners above, normal, exponential and other
distributions, ChaCha8 and every cryptographic generator (secrets use `host.random`
directly), weighted choice, sampling without replacement (shuffle a copy, take a prefix), and
32-bit outputs.

## Laws (planned for the build lane, `core/std/tests/rand.hirsel`)

- **R1 Stream vector.** `new(1, 2)` then 20 `next`s equals Go `TestPCG`'s 20 values
  (`pcg_test.go:56-79`, first `0xc4f5a58656eef510`). Oracle: Go's published table. Bug
  caught: the 128-bit multiply drops the `hi*mulLo + lo*mulHi` cross terms, or DXSM uses
  the old state rather than the new one.
- **R2 Bounded draws.** One `new(1, 2)` stream, 20 `below` calls with `n` cycling through Go's
  `uint64s` table (`regress_test.go:33`), equals the 20 `Uint64N(..)` rows of
  `regressGolden`. Oracle: Go's golden table. Bug caught: the threshold computed as
  `n % n`, a power-of-two path that also rejects, or `lo <= t` in place of `lo < t`.
  Either one desynchronizes every later row.
- **R3 Rejection-heavy draws.** For `n = 2^63 + 1` (where almost half the draws are
  rejected) and `n = 2^64 - 1`, 20 `below` values from `new(7, 11)`. Oracle:
  `core/tools/pcg_reference.py`, a Python port of `pcg.go` and `rand.go:uint64n` written from
  the Go source. Before it emits these rows, it checks itself against R1 and R2. Bug caught:
  a rejection loop that redraws without recomputing `lo`.
- **R4 Shuffle.** `shuffle` of `[0, 1, .., k-1]` on one `new(1, 2)` stream, with `k` cycling
  through Go's `permSizes`, gives the 20 `Perm(..)` rows of `regressGolden`. The first `Perm(5)`
  is `[1, 4, 2, 0, 3]`. Oracle: Go's golden table. Bug caught: the loop runs `i` upward, or
  draws `below(p, i)` (Sattolo's off-by-one, which never leaves an element in place).
- **R5 Float.** The 20 `Float64()` rows of `regressGolden` (`regress_test.go:276-295`, first
  `0.6764556596678251`). Compare bits, not decimal text. Oracle: Go's golden table. Bug
  caught: the top 53 bits taken (Goose's rule), or a division by `2^53 - 1`.
- **R6 Resume.** After 5 draws, `new(state(p))` continues with the same 15 values R1 lists
  next. Oracle: R1's table. Bug caught: `state` returns `(lo, hi)`.
- **R7 Zero bound.** `below(g, 0)` fails Bounds (`= fail Bounds`). Oracle: the contract. Bug
  caught: a `-n % n` division by zero surfacing as a different cause.

Prior vectors: `c5690b37^:conformance/SEM/CAP-1-random-draws-run.hir` and
`771885fb^:stdlib/std@1/random.hir`. The old module used a different generator, so its
values are not oracles here. Only its rejection-edge cases (n = 1, n = 2^64 - 1) carry over,
recomputed with `pcg_reference.py`.
