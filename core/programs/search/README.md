# Lexical search

`search@1/search` ranks the rows of a `store` table by BM25. This is form B
of a search index (STORE-SPEC §1.2 "Search form B"): a store aggregate of
statistics on the table, and a store projector that folds the table's rows
into generations of block postings, which a query reads document at a time
with block-max bounds.

## Declaring

Call `search.declare(mut d, mut t, name, stop_words, terms)` between
`store.declare_table` and `store.finish`. `terms` is a described function
`fn(K, V) -> Result[search.Terms, Capacity]`, normally

```hirsel
fn body_terms(_id: u64, doc: Doc) -> Result[search.Terms, Capacity] {
    Ok(search.analyze(search.tokenizer(stop_words()), doc.body))
}
```

Its analyzer must use the same `stop_words` that `declare` receives, because
`query` tokenizes query text with them.

`declare` puts these records on the table:

- `name.stats`, an aggregate with one group for the corpus and one per
  term. The corpus group counts the rows and sums their token counts; a
  term's group counts the rows that hold it, its document frequency. It
  changes in the same transaction as the row. The store records the
  revision of `terms`, so a changed analyzer, row codec or default rebuilds
  it at the next `Migrate`.
- `name`, a projector over the table, and its tables `name.blocks`,
  `name.postings`, `name.heads` and `name.mentions`. Each generation holds, per term,
  blocks of up to 128 postings in key order: a header with the block's last
  key and, per distinct term frequency, the least token count of a row with
  it; the block's postings packed into one byte string (offsets, term
  frequencies and token counts, eight-byte key heads, then the keys); and
  each term's newest block number, so a fold appends without a range read. It also holds
  each row's mention (its token count and terms), or a mask where the row
  was deleted.

The application or the runner's head steps the projector with
`store.project_step(mut db, name, limits)` (STORE-SPEC §1.14 "Driving").
Rows written since its last published generation are its tail, which a
query scores from the rows themselves, so a query always sees every row its
transaction sees, its own writes included. `search.rebuild(mut db, s)`
rebuilds the statistics and requests a rebuild of the generations;
`store.index_build_step` finishes the statistics. `statistics(s)` returns
the store's aggregate handle.

## Tokenizer

The analyzer segments Unicode scalars using a lexical subset of the Unicode
18.0 UAX #29 word boundary rules. It joins letters and decimal numbers,
consecutive Katakana, and runs bridged by ExtendNumLet characters such as
underscore. Word_Break mid-letter and mid-number punctuation joins compatible
neighbors. A single quote can end a Hebrew word, and double quotes can bridge
Hebrew letters. Extend marks stay in the word and do not affect neighboring
boundaries. Format and ZWJ characters are skipped. Unclassified letters
and decimal digits each form a word, with their following Extend marks.
Other punctuation, emoji, and whitespace separate words. A connector-only
run produces no token. The analyzer does not use dictionary segmentation.

After segmenting, the analyzer applies full default, locale-independent case
folding using mappings C and F in Unicode `CaseFolding.txt`. For example,
`Straße` becomes `strasse` and final sigma becomes sigma. It applies no
canonical normalization, accent stripping, or stemming. Each stop-list
entry is segmented and folded the same way, and each resulting word is removed
from both document and query tokens. Pass an empty list to keep every token.
Postings store term frequency. They omit positions.

## Querying

Call `search.query(tx, s, text, k, params, allows)` with a `store.Tx` from
`store.run` or `store.look`, so entry and query bodies can call it. It needs
no `waits`. Only allowed rows rank. To filter on the row's value, fetch the row in `allows` through the
same `tx`. `allows` is asked only for rows that would enter the top `k`, so
it may be asked fewer times than there are matches. The query returns at
most `k` hits, best first. Missing query terms
contribute nothing, and repeated query terms score once.

Each term contributes

```text
log(1 + (N - df + 0.5) / (df + 0.5)) *
tf * (k1 + 1) / (tf + k1 * (1 - b + b * dl / avgdl))
```

`N`, `df` and `avgdl` come from every row the transaction sees, rows with no
terms included. Eligibility changes which rows can rank. It does not change
their scores. Defaults are `k1 = 1.2` and `b = 0.75`. The `bm25` constructor
accepts finite `k1 >= 0` and `0 <= b <= 1`.

Equal scores rank in increasing primary key order, the order of the table's
key codec bytes. One store state therefore gives one result.

While the statistics build, a query answers `NotReady` with their name.
Before the projector first publishes, a query scores every row.

`search.rank(texts, text)` ranks a list of strings without a store. A hit's
key is the string's position.

## Cost

A write analyzes the old and the new row and changes the groups of the
terms that differ. The corpus group is one row, so every insert, delete and change of
a token count rewrites it, and the store's one writer serializes those
writes. A projector step folds a page of changed rows into a generation,
appending each row's postings to its terms' last blocks; a merge rewrites
its inputs' newest mentions into one generation.

A query reads one group per distinct query term and scores the tail's rows.
Then, over each generation oldest first, it reads the query terms'
postings in windows, each ending at the least last key of the blocks the
terms stand in (block-max, as Lucene's MaxScoreBulkScorer reads it). A
window whose blocks' bounds, summed in query order, cannot reach the `k`-th
score is passed without reading a posting. Otherwise the terms split as
MaxScore splits them: those whose bounds together cannot place a row are
read only at rows the others hold, and only when the row can still enter.
Bounds are exact: a block's bound is the greatest contribution over its
frontier of term frequencies and least token counts, and a bound equal to
the `k`-th score prunes only rows whose keys rank after its key, so pruning
never drops a row that form A would rank. A row that would enter the top
`k` is checked against the tail's keys and every newer generation's
mentions before `allows`.

At 1,000,000 rows, top 10, a query takes 5.5 to 41 times fewer steps than
form A on the STORE-SPEC v2.144 queries. Step the projector often: every
query reads and scores the whole tail, at about 8,600 steps per changed row
at that size, so 1,000 unprojected rows cost more than form A's whole query.

## Laws

- `snapshot_bm25` checks fixed Python reference scores to an absolute
  tolerance of `1e-9`. It covers exact filtering, ties, a row with no terms,
  duplicate query terms, deletes, overwrites, aborted writes and retained
  snapshots.
- `filtered_top_k` checks every eligibility mask for the six-row fixture and
  every limit from zero to seven against the Python ranking.
- `bm25_histories` runs seeded histories of puts, overwrites with the same
  body, deletes of present and absent rows, rebuilds and build steps. Each
  query runs inside the writing transaction or through a look after it, and
  must equal the model's brute force: every present row tokenized and scored
  with arithmetic written in the test, sorted by score and then key, and cut
  at `k`. The law fails if no query had a tie across the `k` boundary, none
  met a build, or no row had zero terms.
- `block_max_differential` runs seeded histories over 2,400 rows with a
  skewed vocabulary, so a term spans several blocks, with projector steps
  that stop mid-pass and mid-merge, rebuilds, filters and `k` from 0 to 129.
  Each query, inside the writing transaction or through a look, must equal
  the same brute-force ranking exactly.
