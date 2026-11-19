# JSON hand derivation

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

The idiomatic reader calls std.json.parse_tree. Its preorder hook still marks
each node admission, and it constructs the workload's owned Json enum. The
tree's inline size is 8 + max(1, 24, 24, 24, 24) = 32. The list grows to four
slots and charges 4 × 32 = 128. The root and both item objects each grow to
four map slots and charge 4 × (24 + 32 + 16) = 288 each.

The two root keys, two name keys, one retained x key, two string values and
one number spelling are eight separate owners. Each charges 8 bytes. The
key items has capacity 5, the other keys grow to 4, and a and b/newline/lambda
fit capacity 4. The number -2.5 is copied at exact capacity 4. The root holds
128 + 3 × 288 + 8 × 8 = 1,056 bytes when parsing finishes. The second x key
adds a transient 8-byte charge before the duplicate insertion drops one key.
Both valid parsers admit those same payload bytes. At document2.node3 the
root's items key charges 8, the root map and items list are still empty, and
the third preorder value is refused before its object is read. The synthetic
site has a request of 0 bytes.

For `{"abcdef\q":0}`, the former reader appended the six-byte key
prefix before reading the escape. Growth from zero requested
round8(max(6, 4, 0)) = 8 bytes. The std reader sees the invalid escape at
byte 9 while validating the key. It never creates the key and charges
0 payload bytes. A missing colon after a valid key is likewise detected
before the key is copied. The same validation-before-copy rule applies to
string values.

For `{"x":1e999+2}`, the owned x key grows to 4 and charges 8. The map
stays empty and the rejected number spelling never allocates. The earlier
parser checked the prefix 1e999 and reported InvalidNumber. The std scanner
validates all seven number-like bytes first and finds the invalid plus at
document byte 10, so the workload reports Syntax@document1. For the
idiomatic error label alone, the old 13-byte prefix charged 16, then grew to
capacity max(22, 2 × 13) = 26 and charged 32. Adding the document digit fits
26. The new six-byte prefix charges 8, then grows to 15 on adding @document
and charges 16. Adding the digit requires 16 bytes, so capacity doubles to 30
and charges 32. Both leave 32 bytes live and briefly hold 48 bytes at their
largest growth. The respective label admission sums are 48 and 56 bytes.

### JSON output

Both layouts now use std's writer. The small fixture still grows the pretty
output through capacities 4, 8, 16, 32, 64 and 128. The first brace charges
8. After the newline and two-space indent the length is 4. The opening
quote grows capacity to 8. Appending the five letters in items needs 10
bytes and grows capacity to 16, as the former per-scalar writer did on
the ninth byte. The later appends fit within the next doubled capacity.
The growth charges are 8 + 8 + 16 + 32 + 64 + 128 = 256 bytes, with a peak
of 64 + 128 = 192 during the last growth. The final 115-byte output holds
128 bytes of capacity. Under the query's `query1=` prefix, old and new
writers append the opening quote, b, the two-byte newline escape, the
two-byte lambda and the closing quote in the same sizes.

For an empty output carrier and a string of 16 plain ASCII bytes, the
former writer admitted capacities 4, 8, 16 and 32, charging 64 bytes in
all. It briefly held 16 + 32 = 48 bytes. The std writer opens with a
four-byte capacity, then appends the full run at length 1. Its next
capacity is max(17, 8) = 17, which charges 24. The closing quote needs
18 bytes and thus doubles capacity to 34, which charges 40. The new
admission sum is 8 + 24 + 40 = 72 bytes and the peak is 24 + 40 = 64.
Under a 31-byte bound, the former writer refused 32 with 15 available
after its first 16 output bytes. Std refuses 24 with 23 available after
the opening quote. Both write the same 18 bytes when admitted.

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
These charges follow from the collection sizes and growth rules above.
