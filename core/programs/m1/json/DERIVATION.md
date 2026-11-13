# json: hand derivation

Preorder visits the outer object, items array, first object, first name,
second object, second name, null, true and n. The second name decodes into
`b`, U+000A, U+03BB. Its UTF-8 bytes are 62 0a ce bb. Inserting x a second time
replaces null with true without moving its position after name. Query steps
items, 1, name select that string. Quoting replaces the newline with backslash-n
and keeps lambda as UTF-8. The fully rendered document is the 115-byte text below.
Its line byte lengths excluding newlines are 1, 12, 5, 17, 6, 5, 22, 15, 5, 4, 11, 1;
there are 11 intervening newline bytes. The line byte sums are
123, 859, 251, 932, 297, 251, 1572, 918, 253, 201, 526, 125, totaling 6418 with
those newlines. See the literal text for the independent arithmetic.

```text
query1="b\nλ"
documents=1 pretty-bytes=115 sum=6418
acquire=document1
release=document1
```

The complete pretty text, without a final newline in its measured bytes:

```json
{
  "items": [
    {
      "name": "a"
    },
    {
      "name": "b\nλ",
      "x": true
    }
  ],
  "n": -2.5
}
```

## Logical charges

| site | actual own-tree admission at the injection point |
|---|---|
| document2.node3 | 0; the third preorder value is an object not yet populated |

Json has logical inline size 8+max(1,24,24,24,24)=32. An array's initial
four-element block charges 4×32=128. An object grows initially to
4×(String-header(24)+Json(32)+16)=288. Scalars live inline in those owners;
null and booleans allocate nothing. Every owned key or scalar lexeme/string
charges its String GROWTH, rounded to 8. A duplicate key does not grow the map;
its old Json payload and temporary duplicate key are cleaned.
The corpus's per-node injection is synthetic for this representation as well as
for a future compact blob: it precedes parsing, not a fixed-size node allocator.
No matched source is fabricated before the compact-sequence rules land.
The matched variant, its sizing pass and revised fixture remain a separately
requested change once that specification commit is supplied.

All charges use MEM-24, charge-v1. Empty owners and static literals charge 0.
A growth block has capacity max(needed,4,2×old), and is admitted before the old
block is refunded. Named injections are synthetic source sites, not host-OOM
or allocator-call counters. Output and instrumentation charges are separate
from the payload charges above. These are hand derivations, not executed Hirsel
results.

## Source length

Physical lines, including comments and blanks: 442 idiomatic, pending matched; Rust entry + shared program: 361 idiomatic and 438 matched. Shared support is counted separately in the lane report.
