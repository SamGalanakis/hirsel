# Vector search

`vector@1/vector` finds the rows of a `store` table nearest to a query
vector. Vectors are `List[i8]` codes. It has two forms. Form A is an exact
flat scan for small tables, up to about 10,000 rows. Form B is an IVF index
with RaBitQ codes for larger tables. It is read through a projector and
reranks its candidates exactly (Store 3.2).

## Form A

### Declaring

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

### Querying

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

### Cost

A query scans the whole index once and scores every code: `O(n · d)` for
`n` rows of `d` elements. It keeps the `k` best hits in order, which costs
`O(n · log k)` comparisons and up to `O(k)` moves per admitted hit. The scan
records one read span over the whole index, so a transaction that queries
conflicts with any concurrent write to the table.

## Form B

### Declaring

Call `vector.declare_ivf(mut d, source, name, metric, code, shape)` after
`store.finish`, with the finished source `Table`. `shape` is
`Shape{dims, clusters, steps}`: every code has `dims` elements, the index
trains `clusters` centroids, and every commit that folds rows is bounded at
`steps`. `dims` of 0, `dims` above 4096 once padded to a power of two (at
least 8), or 0 clusters are refused with `Shape(shape)`. The call declares
a projection `name` (its tables are `name.blocks`, `.mentions`, `.fill`,
`.gens`, `.lineages` and `.rotated`) and registers its projector. The
store's head steps it, and `store.project_step(mut db,
vector.ivf_name(v), limits)` runs the same step.

The projector folds the source into immutable generations (STORE-SPEC
§1.14). A generation encodes each row once:

- It finds the row's nearest centroid by L2, the lowest index on a tie.
- It rotates the residual with three rounds of fixed random signs and an
  integer Walsh-Hadamard transform, over the code zero-padded to `D`
  elements.
- It keeps the residual's sign bits, packed 32 vectors to a block (one byte
  per vector per 8 dimensions, as the kernel reads them), with the
  estimator's factors.
- It keeps a 4-bit extended code (Extended RaBitQ) in the key's mention.

A delete is a mask. A merge writes each key's newest mention among its
inputs and encodes the row again under the output's centroids. It writes a
mask only when the merge may not drop masks.

Centroids are trained once, by 8 rounds of k-means over the first
`32 · clusters` source rows in key order. Training runs at a generation's
first write, when no live generation has trained centroids and the source
has at least `4 · clusters` rows. Until then, a generation uses one zero
centroid. A newer generation takes the newest trained centroids. The index
does not retrain as the data drifts.

### Querying

Call `vector.query_ivf(tx, v, query, k, probes)`. It returns the same kind
of hits as form A: exact scores, nearest first, with equal scores in
increasing primary key order. The query:

1. Reads the tail (`store.read_tail`) and scores its present rows exactly.
2. For each generation, ranks its lineage's clusters by the query (by
   inner product for `Dot`, by distance for `L2`) and reads the best
   `probes` of them.
3. Per probed cluster, quantizes the rotated query residual to 4 bits and
   builds a lookup table. The integer FastScan kernel sums it over each
   block's sign planes, 32 vectors at a time.
4. Takes each vector's 1-bit estimate plus RaBitQ's error bound (ε0 = 1.9)
   as an upper bound. A vector whose bound cannot beat the 10 · k best
   extended estimates so far is pruned.
5. Skips a key the tail holds or a newer generation mentions. A survivor
   is scored with its 4-bit code.
6. Reranks the 10 · k best estimates exactly from their rows, read with
   `store.fetch_many`, and merges them with the tail's rows.

Until the projection has published, or when the Tx has no pin at its image
(a follower), the query scores every row exactly. A query of another
length is refused with `Dimensions(query length, dims)`. A code of another
length is refused when a row is folded (`Store(Limit(Value))`) or scored
(`Dimensions`).

Tiered sources wait on D7. A query on a Tiered source is refused with
`Store(Tiered(Tail))`.

### Cost

The kernel takes about 33 steps per vector at 768 dimensions. A query costs
the tail's rows, `probes` clusters per generation, one mention read per
survivor and one batched fetch of 10 · k rows. A query in a `run` records
the read of the projection row and of the tail's span. Its result is
approximate: rows outside the probed clusters are not seen. The kill-check
numbers are in the lane report (store-vector-b-3-2-2774).

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
- `ivf_recall` puts form A and form B on one table: 2,048 rows of 32
  dimensions around 16 centres, probed 4 clusters at a time. For both
  metrics it checks four store states: before the first publication, after
  it, with a tail (the best row deleted, and another row moved onto the
  query), and after churn folded into delta generations and merged. In
  each state, a run and a look give one list. Every hit is a present row
  at its exact score, in rank order. Recall@10 against form A's k-th score
  is at least 0.95. Before publication, the list is form A's exactly. In
  the tail state, the moved row is first under L2.
