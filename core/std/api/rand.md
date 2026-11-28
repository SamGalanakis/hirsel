# Rand

[API overview](../API.md). [Types and operation contracts](../rand.hirsel).

`std.rand` implements the deterministic stream from Go's `math/rand/v2` PCG-DXSM.
A seed always gives the same stream on every target and release. All operations
are allocation-free. Use the stream for simulation, sampling, test data, load
balancing and jitter. For keys, tokens and unguessable identifiers, use bytes
straight from `host.random`. A known seed or enough stream outputs can reveal
future outputs.

`Pcg` is plain ([value kinds](../../../docs/spec/ownership.md#own-3-kinds)). `let fork = g`
copies the state and forks the stream. Draws borrow it `mut`. Each generator
belongs to the caller and uses no implicit global or thread state.

## Seeding

- A store call can use `std.rand.new(info.seed, 0)` with its `CallInfo.seed`.
  Each call gets a different seed and replay uses that same seed. Tests and other
  reproducible callers can supply their own constant or derived words.
- For fresh entropy, use a buffer with capacity 16 and call
  `host.random(r, mut b)` with an explicit `Random` capability. The draw overwrites every byte
  starting at zero and sets the length to capacity without allocating. A buffer with
  zero writable capacity raises `Bounds` before issue. Pass the result to `std.rand.new(list.word_le(b, 0), list.word_le(b, 8))`.
  Recording captures the host bytes at delivery and replay supplies them again
  ([the replay point](../../../docs/spec/capabilities.md#cap-8-the-replay-point)).
  Subsequent draws only use the generator state.

A signed range `[lo, lo + size)` is `lo + int.truncate(std.rand.below(mut g, size))`
when the size and the range fit `i64`. A list choice is
`list.get(xs, std.rand.below(mut g, list.len(xs)))`. A Bernoulli trial is
`std.rand.below(mut g, n) < k` for probability `k/n`.

## Stream compatibility

The reference is Go `46b62495`, `src/math/rand/v2/pcg.go` and `rand.go`.
The recurrence and DXSM output match Go. Bounded draws always use Go's 64-bit
Lemire reduction, including on 32-bit targets. This preserves draw consumption
across platforms. Go's golden vectors cover the stream, bounded draws, shuffles
and the low-bit float mapping. [Go source](https://github.com/golang/go/tree/46b62495/src/math/rand/v2).
[Contract tests](../tests/rand.hirsel).

Goose uses one-word SplitMix64, modulo reduction with rejection, and the high
bits for floats. Each choice changes the stream or how its draws are consumed.
Hirsel's store uses SplitMix64 to derive call seeds. A future stream algorithm
would have its own type, so existing seeds and resumes keep their meaning.
