# Lexical search

`search@1/search` ranks the rows of a `store` table by BM25. This is form A
of a search index (STORE-SPEC §1.2 "Search form A"): a store entry index of
postings and a store aggregate of statistics on the table, for tables up to
about 10,000 rows.

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

`declare` puts two records on the table:

- `name`, a `Many` index with one entry per distinct term of a row. The
  entry's key is the term, its frequency in the row and the row's token
  count. Every row also has one corpus entry.
- `name.stats`, an aggregate with one group for the corpus and one per
  term. The corpus group counts the rows and sums their token counts; a
  term's group counts the rows that hold it, its document frequency.

Both change in the same transaction as the row, so a query sees the
transaction's own writes. There is no write call to forget. The store
records the revision of `terms`, so a changed analyzer, row codec or default
rebuilds both records at the next `Migrate`. `search.rebuild(mut db, s)`
rebuilds them on a serving store, and `store.index_build_step` finishes
them. `posting_index(s)` and `statistics(s)` return the store's handles.

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
no `waits`. `allows` receives each candidate row's key once, and only allowed
rows rank. To filter on the row's value, fetch the row in `allows` through the
same `tx`. The query returns at most `k` hits, best first. Missing query terms
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

While either record builds, a query answers `NotReady` with that record's
name.

`search.rank(texts, text)` ranks a list of strings without a store. A hit's
key is the string's position.

## Cost

A write analyzes the old and the new row. A posting's key holds the row's
token count, so a write that changes the count rewrites every posting of
the row; otherwise only the postings and groups of the terms that differ
change. The corpus group is one row, so every insert, delete and change of
a token count rewrites it, and the store's one writer serializes those
writes.

A query reads one group per distinct query term and one posting range per
term it finds. It does no WAND or block-max pruning, so its work grows with
the postings it visits. For `H` allowed matches the top-k heap takes
`O(H log(1 + min(k, H)))` work and holds at most `k` hits. The query also
keeps one accumulator per distinct candidate. Store and runtime admission
failures propagate rather than truncating the result.

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
