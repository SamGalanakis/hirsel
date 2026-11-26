# Vector search

`vector@1/vector` finds the rows of a `store` table nearest to a query
vector. Vectors are `List[i8]` codes. It has two forms. Form A is an exact
flat scan for small tables, up to about 10,000 rows. Form B is an IVF index
with RaBitQ codes for larger tables. It is read through a projector and
reranks its candidates exactly (Store 3.2). Both forms take a prefilter
bitset that restricts a query to some rows before it scores them
(Store 3.3).

## Form A

### Declaring

Call `vector.declare(mut d, mut t, name, metric, code, ordinal)` between
`store.declare_table` and `store.finish`. `code` is a described function
`fn(K, V) -> Result[List[i8], Capacity]` that returns a row's vector. Every
row of the table has a vector. `ordinal` is a described function
`fn(K) -> u64` that gives a row's bit in a prefilter (see Filtering).

The index is a store covering index named `name` on the table. Its payload
is the row's code. It changes in the same transaction as the row, and a
query sees the transaction's own writes. The store records the index's
revision, so a changed `code` function, row codec or default rebuilds the
index at the next `Migrate`. While the index builds, a query returns
`Store(NotReady(name))`. `vector.entry_index(v)` returns the store's handle
for store operations such as `store.rebuild_index`.

### Querying

Call `vector.query(tx, v, query, k, filter)` with a `store.Tx` from
`store.run` or `store.look`, and `Every` or a prefilter. It returns at most `k` hits, nearest first. Each hit holds the
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

Every allowed row's code must have the query's length. One whose code does
not refuses the query with `Dimensions(query length, code length)`.

### Cost

A query scans the whole index once and scores every code: `O(n · d)` for
`n` rows of `d` elements. It keeps the `k` best hits in order, which costs
`O(n · log k)` comparisons and up to `O(k)` moves per admitted hit. The scan
records one read span over the whole index, so a transaction that queries
conflicts with any concurrent write to the table.

## Form B

### Declaring

Call `vector.declare_ivf(mut d, source, name, metric, code, ordinal, shape)` after
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

Centroids are trained once, by 8 rounds of k-means on a reservoir of up to
`32 · clusters` rows sampled across the whole source. The projection name
seeds the sampler, so rebuilding the same source under the same name uses
the same sample. Training starts with the first sampled row as a centroid,
then takes the sampled row farthest from its nearest chosen centroid until
it has `clusters` seeds. Training runs at a generation's first write, when
no live generation has trained centroids and the source has at least
`4 · clusters` rows. The training commit's step budget covers the scan
and clustering. Until training, a generation uses one zero centroid.
A newer generation takes the newest trained centroids. The index does
not retrain as the data drifts.

### Querying

Call `vector.query_ivf(tx, v, query, k, probes, filter)`. It returns the same kind
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

## Filtering

A query's `filter` is `Every`, or `Only(bits)` with a `std.bitset.BitSet`.
A row is allowed when its ordinal, `ordinal(key)` as declared, is below
`std.bitset.len(bits)` and its bit is set. An ordinal past the end is not
allowed, so an empty bitset allows no row. The ordinal reads only the key:
a row keeps its ordinal across overwrites, the tail and every generation,
and nothing stores it. A changed `ordinal` function applies to the next
query, with no rebuild.

The filter applies before scoring, not after the cut:

- Form A skips a disallowed entry before it checks or scores its code.
- Form B skips a disallowed tail row before it scores it. The key still
  masks the generations, so the row's old codes are not read either. A
  generation's slot is tested after its 1-bit bound and before the masks,
  the 4-bit estimate and admission, so the `10 · k` candidates and the
  exact rerank are all allowed rows. With no image, the exact scan skips a
  disallowed row before it reads its code.

When the scan is exhaustive, a filtered result is therefore the unfiltered
result with the disallowed rows removed. Form A is always exhaustive. Form
B is exhaustive with no image, or when it probes every cluster and the
allowed rows number at most `10 · k`. Otherwise a selective filter only
improves form B's recall, since the candidates it would have spent on
disallowed rows go to allowed ones. There is no index per filter value.
A filtered form B query still reads every probed block: the cost of a
query does not fall with the filter's selectivity.

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
  the tail state, the moved row is first under L2. The law also imports two
  populations in key order and checks that a single probe returns the
  brute-force nearest row for a held-out query from the later population.
- `prefilter` puts both forms on a 512-row table with the non-identity
  ordinal `3 · key + 1` and a 1,000-bit filter that allows 26 rows (keys
  that would match past the bitset's end are not allowed). Every query
  probes all 16 clusters. Under L2 and Dot, before publication, after it,
  and in a run whose own writes delete the filtered best row and move an
  allowed row, a disallowed row and a row past the bitset onto the query:
  both forms' unfiltered lists over every row agree, and each form's
  filtered top 4 and top 10 equal its unfiltered list without the
  disallowed rows. The same holds for an empty filter. The law fails if no
  unfiltered top k held a disallowed row.
