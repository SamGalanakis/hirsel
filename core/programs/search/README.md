# Lexical search

`search@1/search` is an inverted index over store. Choose a table namespace and a
codec metadata type `M`. Bind `search.spec(name)` to `search.Spec[M]`. Call
`search.install(mut migration, spec)` inside a store migration, then call
`search.open(schema, spec, take analyzer)` with that same specification. The
analyzer fixes tokenization for the handle. Use the same stop words every time
you open that namespace. Rebuild the index to change its stop words.

Call `search.upsert(mut tx, index, id, text, take metadata)` or
`search.delete(mut tx, index, id)` in a `store.run` callback. Propagate a write
error from the callback so that store aborts the entire edit. A commit publishes
the document's typed metadata and token count, its distinct terms, its postings,
and the updated statistics together. Deleting an absent ID returns false.
An empty document counts towards corpus size and contributes zero tokens.

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

## Ranking a snapshot

Call `search.query(snapshot, index, text, k, params, allows)`. The predicate
receives the candidate's numeric ID and typed metadata from that snapshot.
It can capture a precomputed allowed set and test the ID against it. The
query calls it once per candidate. It scores only eligible documents and
returns at most `k` matching hits. Missing query terms contribute nothing.
Repeated query terms score once. Equal scores sort by increasing document ID.

Each term contributes

```text
log(1 + (N - df + 0.5) / (df + 0.5)) *
tf * (k1 + 1) / (tf + k1 * (1 - b + b * dl / avgdl))
```

`N`, `df` and `avgdl` come from the whole snapshot. Eligibility changes
which documents can rank. It does not change their scores. The filtered
top-k is the reference ranking of eligible documents scored with whole-corpus
statistics. Defaults are `k1 = 1.2` and `b = 0.75`. The `bm25` constructor
accepts finite `k1 >= 0` and `0 <= b <= 1`.

Updates touch only the old and new document's terms, with store tree operations
for individual rows. Queries use an exact posting range per distinct indexed
query term and one metadata lookup per candidate. They exhaust those ranges
without WAND or block-max pruning. The caller's charged work grows with
postings visited. For `H` eligible matches the top-k heap takes
`O(H log(1 + min(k, H)))` work and holds at most `k` hits. The query also
retains one accumulator per distinct candidate. Store and runtime admission
failures propagate rather than truncating the result.

`snapshot_bm25` checks fixed Python reference scores to an absolute tolerance
of `1e-9`. It covers exact filtering, ties, duplicate query terms, deletes,
updates, aborted writes and retained snapshots.
`filtered_top_k` uses `std.prop.all` to check every eligibility mask for the
six-document fixture and every limit from zero to seven against the Python
ranking.
