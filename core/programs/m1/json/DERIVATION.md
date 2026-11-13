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

Idiomatic Json's inline size is 8+max(1,24,24,24,24)=32. Its initial
four-element array block charges 128, and a four-entry Map[String, Json]
charges 4×(24+32+16)=288. At document2.node3 the third preorder value is an
object not yet populated, so no own-node allocation occurs at that point.
It is the corpus's synthetic preorder refusal and selects only idiomatic.

The matched variant reads the frozen input twice through std.json.parse: its
validating size pass and its writing pass. It stores every duplicate member in
one indexed compact document. ObjectIndex supplies the first position with the
last value. With u32 widths and one-byte tags, the small document is:

- First item object: header 9 + index 4 + key name 8 + Str(a) 6 = 27.
- Second item object: header 9 + index 12 + name member (8+9) + x/null (5+1)
  + x/true (5+1) = 50. The decoded b/newline/lambda text holds four UTF-8 bytes.
- Items array: header 9 + index 8 + 27 + 50 = 94.
- Root: header 9 + index 8 + items member (9+94) + n member (5+9) = 134.

The region admits round8(134)=136 exactly once. text.as_string makes only a
view and admits nothing; no decoded String tree or intermediate blob exists.
Auxiliary ObjectIndex ordinal lists are outside the document: two ordinals
charge 8, one charges 8, and the three raw members of the second object charge
16. Those indexes are local to printing/querying and close before their views
could escape. Query returns owned output, not a view anchored to a local index.

For capacity-blob, document 1 publishes and contributes its query, 115 pretty
bytes and sum 6418. Document 2's validating size pass produces 134, and its
136-byte reservation is rejected before any value is written. Only injection
runs call the workload's size wrapper; normal runs avoid a redundant third
pass. Document 2's region closes empty and its resource releases once:

```text
query1="b\nλ"
documents=1 pretty-bytes=115 sum=6418
acquire=document1
release=document1
acquire=document2
release=document2
failure=Capacity@document2.blob
```

The Rust matched blob retains its postorder u32-offset encoding, whose small
size is 109 bytes. A size accumulator includes even the overwritten duplicate's
already-written value prefix, so the one reservation is exact for that encoding.
The no-growth publication test covers the duplicate/UTF-8 small case and every
normal document; output parity is checked by the independent Python oracle.
The two formats are both one-blob representations and have different headers.
No new runtime or compiler-cost ratio is claimed from their earlier baselines.

The tree and blob entries print container queries with the existing corpus
oracle/Rust pretty rendering. NINE.md's description calls them compact; scalar
query fixtures agree either way. This follows the executable corpus contract,
without inventing a new output spelling.

All charges use MEM-24, charge-v1. Empty owners and static literals charge 0.
Named injections are source sites, not native allocator call numbers. Trace
strings, resource grants and output blocks are separate from the payload charges.
These are hand derivations; no Hirsel workload has executed yet.

## Source length

Physical lines, including comments and blanks: 442 idiomatic and 185 matched;
Rust entry plus shared program.rs: 376 idiomatic and 506 matched.
Shared support is counted separately in REPORT.md.
