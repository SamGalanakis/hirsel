# Hybrid retrieval, exact

`hybrid@1/hybrid` fuses ranked result lists into one ranking by reciprocal
rank fusion. A typical use fuses a `search` query and a `vector` query over
the same `store` table. It is a pure library: it reads no store and depends
only on `std`.

## Fusing

Call `hybrid.fuse(key, c, lists, k)`. `lists` holds the keys of each input
list, best first. `key` is the key codec of the rows, usually the table's:
`store.key_codec(t)` for the table, or `store.primary_key(ix)` for an
index on it. It returns
at most `k` hits, best first. Each hit holds the row's key and its rank in
each list, from 1, or 0 where the list does not hold the row.

A row's score is the sum of `1 / (c + rank)` over the lists that hold it.
`c` is the usual RRF constant; 60 is common. Any `u64` is allowed, and
`c + rank` never overflows.

A row's rank in a list is its first position there. A repeat of a key later
in the same list adds nothing.

## Exactness

Scores are compared as exact fractions, never as floats, so a fused order
does not depend on rounding. Equal scores rank in increasing order of the
key's encoded bytes under `key`, so one set of lists gives one order.

For example, with `c = 1`, a row ranked 2nd in one list and 5th in another
scores `1/3 + 1/6 = 1/2` and ties a row ranked 1st in one list only. At
`c = 2^64 - 1`, ranks `(1, 4)` score `(2c + 5) / (c² + 5c + 4)` and beat
ranks `(2, 3)`, `(2c + 5) / (c² + 5c + 6)`, by a margin no `f64` can hold.

## Cost

With `n` distinct rows across `m` lists, fusion encodes each input key
once, computes each row's fraction once, and sorts the `n` rows. Each
comparison multiplies two fractions of at most `2m` limbs each.

## Laws

- `hand_fusions` checks fused orders and ranks worked out by hand: a
  three-way tie at `c = 1`, one of them a sum over two lists; ranks
  `(1, 4)` above `(2, 3)` at `c = 60` and `c = 2^64 - 1`, with ties at
  `1 / (c + 1)` broken by key, not by list order; cuts at `k` inside a
  tied group; and a repeated key counted once.
