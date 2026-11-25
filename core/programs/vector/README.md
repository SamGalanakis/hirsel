# Vector search, exact

`vector@1/vector` finds the rows of a `store` table nearest to a query
vector. Vectors are `List[i8]` codes. This is form A of a vector index: an
exact flat scan for small tables, up to about 10,000 rows.

## Declaring

Call `vector.declare(mut d, mut t, name, metric, code)` between
`store.declare_table` and `store.finish`. `code` is a described function
`fn(K, V) -> Result[List[i8], Capacity]` that returns a row's vector. Every
row of the table has a vector.

The index is a store covering index named `name` on the table. Its payload
is the row's code. It changes in the same transaction as the row, and a
query sees the transaction's own writes. The store records the index's
revision, so a changed `code` function, row codec or default rebuilds the
index at the next `Migrate`. While the index builds, a query returns
`Store(NotReady(name))`. `vector.entry_index(v)` returns the store's handle
for store operations such as `store.rebuild_index`.

## Querying

Call `vector.query(tx, v, query, k)` with a `store.Tx` from `store.run` or
`store.look`. It returns at most `k` hits, nearest first. Each hit holds the
row's key and its exact integer score. A larger score is nearer:

| Metric | Score |
|---|---|
| `Dot` | `Σ q[i] · c[i]` |
| `L2` | `−Σ (q[i] − c[i])²` |

The score is computed in `i64`, so it never overflows: a code of `n`
elements is at most `n · 65025` in magnitude. `vector.score(metric, query,
code)` is the same function.

Equal scores rank in increasing primary key order, the table's key codec
order. One store state therefore gives one result.

Every code must have the query's length. A row whose code does not refuses
the query with `Dimensions(query length, code length)`.

## Cost

A query scans the whole index once and scores every code: `O(n · d)` for
`n` rows of `d` elements. It keeps the `k` best hits in order, which costs
`O(n · log k)` comparisons and up to `O(k)` moves per admitted hit. The scan
records one read span over the whole index, so a transaction that queries
conflicts with any concurrent write to the table.

## Laws

- `exact_top_k` runs seeded histories of puts, overwrites with the same
  code, deletes of present and absent rows, rebuilds and build steps. Each
  query runs inside the writing transaction or through a look after it. Its
  answer must equal the model's brute force: every present row scored with
  arithmetic written in the test, sorted by score and then key, and cut at
  `k`. The law fails if no query had a tie across the `k` boundary, or if no
  query met a build.
- `extremes` checks hand-computed scores at 127 and −128 for both metrics,
  and the `Dimensions` refusal.
