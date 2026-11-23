# Map index (implementation)

This is an implementation document, not part of the language specification. It fixes how a Map
stores and finds its entries so that `core.map`, the code the compiler emits for a Map and every
engine agree on one layout under one normal-form version
([PROG-10](../spec/programs.md#prog-10-code-addresses)). The language promises only what
[SEM-5](../spec/semantics.md#sem-5-maps) and [PROG-11](../spec/programs.md#prog-11-map-index)
say; everything here may change with a new normal-form version. It moved out of the specification
on 2026-11-23 (Sam's Spec Q4 = A).

None of it is observable. No value, admission, refund, step, cleanup order or iteration order
depends on it, on the run's Map key, on the keys derived from it, or on which placement mode a
Map is in ([SEM-5](../spec/semantics.md#sem-5-maps)).

**Carrier.** A Map's carrier is `{data, used, cap}`: the block, the number of initialized entry
slots, and the capacity. While `cap` is 0 the Map has no block and `used` is 0. Then `data` is not
an address: it is 0 for an ordinary empty Map and 1 for a **strong-empty** Map, one whose index
would be strong once it has one. No engine dereferences, frees or charges `data` while `cap` is
0. `map.new` makes `{0, 0, 0}`. `len`, `get`, `update`, `remove`, `reserve(0)`, cleanup and
iteration of a Map with `cap` 0 read only `cap`. A move copies the carrier, so it keeps the
strong-empty mark. The first allocation reads the mark: the new header's strong bit is set when
the Map was strong-empty or its key type is composite (**Classes**).

**Entries are authoritative.** A Map's block holds `cap` entry slots, then a 32-byte index
header, then its index. An entry is the own tuple `(meta: u64, key: K, value: V)`, laid out by
the one entry recipe of the native layout (MEM-13): its fields are ordered by alignment, then
size, then position, so the meta word is not always first. For `Map[u64, u64]` the meta is at
offset 0, the key at 8 and the value at 16, with stride 24. For `Map[String, u64]` the key is at
0, the meta at 24 and the value at 32, with stride 40. Inline construction, relocation, borrowed
and consuming iteration and every cleanup use that one recipe.

Slots below `used` are the **initialized prefix**. Nothing reads a slot at or above `used`. In
the prefix, an entry is **live** when its whole meta word is not 0, and **dead** when it is 0.
Order, membership, iteration, cleanup, compaction, growth, cloning and consuming iteration read
only the live entries of the prefix, in slot order, and clean each key before its value
([SEM-6](../spec/semantics.md#sem-6-iteration)). The index is derived from them, and a rebuild
reconstructs it from them alone.

**Index.** The index has B = max(16, next_pow2(2 × cap)) buckets in G = B / 16 groups. Group g
is 16 control bytes followed by 16 entry indices. An entry index is a zero-based slot number. It
is a `u32` while cap ≤ 2^32, so slot 2^32 − 1 still fits, and a `u64` above that. A group is
therefore 80 or 144 bytes, its stride. Group g's control bytes begin at
cap × S + 32 + g × stride, where S is the entry stride. Bucket j is group j / 16, position
j mod 16. Offsets are computed in 64 bits even where entry indices are `u32`: at cap = 2^32, B
is 2^33.

Group offsets need not be 16-byte aligned. With cap = 5 in `Map[u64, u64]`, the entries end at
120, the header spans 120–152 and group 0 begins at 152, which is 8 mod 16. Every engine reads a
group's 16 control bytes with an unaligned 16-byte load, or byte by byte, and never reads past
them, the last group's included. The block's physical size is
cap × S + 32 + G × stride bytes, computed with checked arithmetic. A size that does not fit in 64 bits is
handled as an allocation the host cannot make. That rule is
independent of the logical charge below.

A control byte is one of:

| byte | meaning |
|---|---|
| `0xFF` | EMPTY: never used since the last rebuild, or erased in a group that holds an EMPTY byte |
| `0x80` | DELETED: erased in a group that held no EMPTY byte |
| `0x00`–`0x7F` | FULL: a tag; the bucket's entry index names a live entry |

When a block is allocated, every control byte is set to EMPTY before any probe. A zeroed page
would read as FULL with tag 0. Every live entry has exactly one FULL byte, and every FULL byte
names a distinct live entry. Non-EMPTY bytes number at most `used`, which is at most cap, which
is at most B / 2, so at least half of every index is EMPTY.

The header holds four words: the live count, the group mask G − 1, the mode, and zero. Mode bit
0 is set in **strong** mode and clear in **fast** mode. Bit 1 is set when entry indices are
`u64`. The width bit follows the capacity. The strong bit is kept independently of it, so growth
that changes the width keeps the mode, and so does a clone that narrows it.

**Capacity bound.** A Map's capacity is at most 2^59, so B ≤ 2^60 and G ≤ 2^56, and bit 56 of a
placement word never selects a group. A grown capacity is clamped to 2^59, and growth or
`reserve` whose need is above 2^59 is refused with `Capacity`, as an admission the current limit cannot make
([MEM-22](../spec/budgets.md#mem-22-the-capacity-value)). `requested` is then the charge of the needed
capacity, saturated at 2^64 − 1. An entry charges at least 16 bytes, so 2^59 entries would
charge 2^63 bytes. The bound therefore refuses nothing that a limit below 2^63 bytes admits, and
it is the same on every target.

**Classes.** A key type's class decides how it is placed in fast mode, and it is fixed by the
type:

- The **word class** holds the key types carried as one non-float scalar: the integer types,
  `bool`, `char`, and records and tuples carried as one such scalar. Its word x is the 64-bit
  word that `map.hash` reads for the key. A signed value is truncated to its unsigned type of
  the same width, then zero-extended, so `-1i8` gives x = `0xff`. A `bool` gives 0 or 1, and a
  `char` its code point.
- The **byte class** holds the non-region record types with exactly one field, where that
  field is a `u8` sequence: `String`, `Bytes`, `List[u8]`, and any user record of that same
  shape. A record whose one field is itself such a record, such as
  `type Name { s: String }`, is not in the byte class.
- Every other key type is **composite**: tuples and records of several fields, enums, and
  wrappers that are not in either class.

The key hash glue and the header use this one predicate. A Map of a composite key type is
strong from its first index, and its code is specialized straight to strong placement, with no
fast operation and no mode test. A Map of a word-class or byte-class key type gets a fast first
index unless it is strong-empty.

**Placement word.** Each key has a 64-bit placement word p under the Map's mode. Its tag is
p >> 57, and its home group is p & (G − 1). A live entry's meta is p | 2^56, which is never 0. A
rebuild takes each entry's tag and home group from its meta. Two placement words are compared
by tag and then by the key itself, never as whole meta words.

In fast mode p is the key's **fast placement word**. Write fold(a, b) for the low 64 bits of the
128-bit unsigned product a × b, XORed with its high 64 bits. Write C for `0x9e3779b97f4a7c15`, and
s0 and s1 for the run's fast salts.

- **Word class.** p = fold(x ^ s0, C).
- **Byte class.** Take bytes b[0..n). Start with z = s0 and i = 0. While n − i > 16, set
  z = fold(z ^ le(i, 8), s1 ^ le(i + 8, 8)) and add 16 to i. Here le(j, w) is the
  min(w, n − j) bytes from b[j], read as a little-endian number with the bytes above zero (0
  when j ≥ n). With r = n − i, let w0 = le(i, 8), and w1 = le(i + 8, r − 8) when r > 8, else 0.
  Then t = fold(z ^ w0, (s1 ^ w1) + n mod 2^64) and p = fold(t, C). An engine reads no byte
  outside b[0..n).

In strong mode p is the key's **strong placement word**: the SipHash-1-3, under the strong key
k_s, of the same structural stream that `map.hash` reads for the key (SEM-5). `map.hash` itself
is unchanged. It is the SipHash-1-3 of that stream under the run key, and it does not place
keys.

**Derived keys.** The run's Map key K is 128 bits the launcher draws from host entropy at
entry, in every run, a replay included. It is not an input at the replay point
([CAP-8](../spec/capabilities.md#cap-8-the-replay-point)), no recording holds it, and a replay
draws a fresh one. Three things are derived from it at entry,
before the first Map operation, with raw SipHash-1-3 under K over ASCII labels, with no length
prefix or padding:

- s0 is the SipHash of the 17 bytes `hirsel.map.fast.0`.
- s1 is the SipHash of `hirsel.map.fast.1`.
- k_s is the 16 bytes le64(a) ‖ le64(b), where a and b are the SipHash of the 19 bytes
  `hirsel.map.strong.0` and `hirsel.map.strong.1`.

They belong to the run, and every task, child and context uses the run's values. No
recording holds K or anything derived from it. Every `map.hash` stream is a whole number of 8-byte words, so no stream equals a
label. Under the PRF assumption for SipHash, k_s, s0, s1 and every `map.hash` output are
therefore independent.

Known answers, with K = `00 01 … 0f`:

| value | answer |
|---|---|
| s0 | `c93577b642b675d3` |
| s1 | `216bdc5358d9c4d5` |
| k_s | `1e7ee605b33f7078d171d0ade994210e` |
| fast word of 0 | `175d5dfbd09e75c7` |
| fast word of 1 | `b08ad43c5024c9bd` |
| fast word of 2^64 − 1 | `d71c61fbd21d7517` |
| fast word of `-1i8` (x = `0xff`) | `488dc5e5cae947bd` |
| fast bytes of the empty sequence | `c601c2084ab71e17` |
| fast bytes of `abcdefgh` | `3251c98ae37d4dbf` |
| fast bytes of `abcdefghi` | `dd8ba4778c150f32` |
| fast bytes of `abcdefghijklmnop` | `441cb7420c58fe33` |
| fast bytes of `abcdefghijklmnopq` | `8d14bb88cfa906fe` |
| strong word of 0 | `e8b70260e432308d` |
| strong word of 1 | `5cd87fd53c86d83a` |
| strong word of 2^64 − 1 | `78ee0e4e0cad36d2` |
| strong word of `-1i8` | `fbe4122ec2e54220` |
| strong bytes of `a` | `9b4f3fea3a32bb74` |
| strong bytes of `abcdefghi` | `687d6af59de6e421` |
| strong bytes of `abcdefghijklmnopq` | `4427af8d97cca0dd` |

The byte class's 8- and 16-byte answers are its boundaries: r = 8 has no w1, and n = 16 takes no
chunk.

**Probing.** From the home group g0, a probe visits the groups g(i+1) = (g(i) + i + 1) & (G − 1).
Because G is a power of two, these are a permutation of the groups in G visits. The group number
is wrapped before its address is computed. In fast mode a probe visits at most min(8, G)
groups: 128 buckets, or 16 × G when G < 8. In strong mode it has no cap. Within a group, buckets
are taken in ascending order.

- A **lookup** compares the key of every bucket whose byte equals p's tag, in probe order, and
  finds the first that is equal ([SEM-7](../spec/semantics.md#sem-7-equality-and-order)). Every
  equal-tag bucket of a group is compared before the group's EMPTY bytes end the search. A
  lookup ends absent after a group that holds an EMPTY byte, or at the cap.
- An **insertion probe** is a lookup that also notes the first EMPTY or DELETED bucket it passes.
  It does not stop at that bucket: it goes on looking for an equal key until the lookup would
  end. Its result is **Found**(entry, bucket), **Vacant**(bucket) or, in fast mode only,
  **Crowded**, when no EMPTY or DELETED bucket lies within the cap. A DELETED bucket noted at the
  cap gives Vacant only after the key has failed to match in every group of the window. The
  result carries p. A bucket or p in a result is valid only for the index it was computed on:
  compaction, growth, a rebuild or a mode change discards it.
- A **placement** takes the first EMPTY or DELETED bucket in probe order. In fast mode it is
  Crowded when there is none within the cap. In strong mode the half-empty index always has one.

Filling an EMPTY bucket adds one to the non-EMPTY bytes and to `used`. Reusing a DELETED bucket
adds one to `used` only. An entry always takes the next slot, `used`. A reused bucket never
reuses an old entry's place in the order.

A fast Map places every key within the cap. So a key that an insertion probe does not find
within the cap is absent, and once the Map is strong a probe again for that key gives Vacant.

**Writers.** Only one bodiless function writes the index: `install(m, b, p, i)` writes p's tag
to bucket b's control byte, writes i at the header's width as the bucket's entry index, and
stores p | 2^56 as slot i's meta. It accepts only a resolved vacancy. The only callers are three
core.map functions:

- `place_entry(m, p, i)` places slot i under the Map's mode, for rebuilds.
- `commit(m, r)` installs a Vacant result r for slot `used`, then calls `advance`.
- `remove` erases. It is the only function that does.

Allocation and clearing initialize controls to EMPTY. `kill` and `take_entry` set a dead
slot's meta to 0. Relocation and clone copy the existing meta as part of moving or copying
the live entry. Only `install` publishes a live entry in the index.

**Insertion.** `map.insert`, `update_or_insert` and the compiler's in-place insertion all follow
these steps:

1. **Empty Map.** If cap is 0, the Map has no index to probe and the key is absent. Make room
   (step 3). A Map with no block never promotes because it has zero groups.
2. **Probe.** Run the insertion probe for the key, under the Map's mode.
   - On **Found**, install the incoming value in place, keep the stored key and its place in the
     order, clean the old value and then drop the incoming key (SEM-5). Nothing is admitted.
   - On **Vacant** with `used` < cap, commit (step 4).
   - On **Crowded** with `used` < cap, promote, then probe again under the strong word. The
     result is Vacant. Commit.
3. **Make room.** If `used` = cap, compact or grow, as **Growth and compaction** below says.
   That discards the probe's bucket and word. Then compute p under the Map's mode, which may
   now be strong, and place. If that placement is Crowded, promote and place again. The
   placement then gives a bucket to commit.
4. **Commit.** Write the key and value into slot `used` (`push_entry`) if they are not already
   there. Then `install(m, b, p, used)` and `advance`: `used` and the live count each grow by 1. `update_or_insert` then calls
   its callback exactly once, on the entry it found or committed.

**Staging and custody** ([MEM-31](../spec/memory.md#mem-31-construction-in-place), a performance
target; a build that does not stage is correct and slower). The compiler's
in-place insertion builds the key, and then the value, directly in slot `used` when that slot
exists before the call (`used` < cap). A built field there is **staged**. Staged fields lie
outside the initialized prefix, so cleanup, iteration and rebuild never read them. They stay
outside it until both operands are complete and the call's step has been taken. Their owner is
the call's pending operand, and each carries an already-in-place flag. Promotion moves no entry
and allocates nothing, so staged fields survive a Crowded probe and its promotion. The single
re-probe then commits them where they are.

When the slot does not exist (`used` = cap), the operands are built in local places and moved
in at commit, after room is made (MEM-31's "any other value"). Making room can move or reallocate
the slots, so the compiler never stages into them.

If the value expression fails after the key is staged, only the staged key is cleaned. If it
fails on the Found path, the previous value is restored. A refused growth leaves the Map, its
old contents and its old owner unchanged. The operands are then still the caller's, and are
cleaned in [FAIL-9](../spec/failure.md#fail-9-pending-results)'s order. The compiler's code branches on
the probe and calls `commit`, `promote` and the ordinary insertion's make-room path. It never
writes a control byte, an index or a meta itself.

**Erasure.** `map.remove` finds its entry and erases the bucket before the entry dies. The byte
becomes EMPTY when its group holds an EMPTY byte, and DELETED otherwise. Then the entry's meta
becomes 0 and the live count drops by one, and the key is cleaned and the value returned
(SEM-5). Erasure never adds a non-EMPTY byte. A consuming loop moves entries out in slot order
and sets each meta to 0 without touching the index. The Map has been taken, so its index is
unreachable and may be stale. Its cleanup reads only the prefix: if the loop is interrupted, the
entries it has not reached are cleaned in order, each key before its value.

**Rebuild.** A rebuild sets every control byte to EMPTY, then calls `place_entry` for each live
entry of the prefix, in slot order, with p taken from its meta. A fast rebuild hashes nothing.
If a fast placement during a rebuild is Crowded, the rebuild stops and promotes. The promotion
itself re-places every live entry. The interrupted rebuild is not resumed, and it returns as
complete to whatever called it. A clone, growth, compaction or `reserve` then goes on from a
complete strong index. An insertion that was pending probes again (step 3).

**Promotion.** A fast Map promotes when an insertion probe, or a placement during an insertion
or a rebuild, is Crowded. Promotion:

1. abandons the whole index, including any partial rebuild;
2. sets the strong bit;
3. sets every control byte to EMPTY;
4. for each live entry of the prefix, in slot order, computes its strong word p, then places and
   installs it, which stores p | 2^56 as its meta.

Every live entry is re-placed exactly once, and no entry is published twice. Promotion allocates
nothing, admits nothing, moves no entry and takes no step
([MEM-30](../spec/budgets.md#mem-30-step-bounds)). Its time is one clear of G groups, one pass over the
`used` slots, and one strong hash of every live key's bytes. G can be far larger than the live
count after `reserve`.

Promotion happens at most once in a Map's life. Nothing clears the strong bit. Growth,
compaction, `reserve` and removal keep it. A clone of a strong Map is strong, whatever its index
width. If the clone is empty, it is strong-empty.

**Growth and compaction.** A Map's logical charge for capacity c is
charge(c) = ceil8(c × (size(K) + size(V) + 16)), as MEM-24 says. It is computed from the logical
sizes, never from the entry stride or the block size. Growth, relocation, free and cleanup all
use this one operand.

- **Make room when `used` = cap.** If cap ≠ 0 and live ≤ cap / 2, compact in place: move the live
  entries to the front in order, set `used` to live, and rebuild. Compaction admits nothing.
  Otherwise grow to max(4, 2 × cap).
- **Growth from c to c'.** Admit charge(c') from the current limit before refunding charge(c) to
  the old record (MEM-26). Copy the live entries in order with their metas, initialize the new
  index to EMPTY, and rebuild under the Map's mode. A refusal raises or returns `Capacity` with
  `requested` = charge(c') and the limit's `available`, and leaves the old block, its contents,
  its charge and its owner unchanged.
- **`reserve(n)` and `try_reserve(n)`.** If live + n > cap, grow to
  max(live + n, 2 × cap, 4), subject to the capacity bound. Otherwise, if `used` + n > cap,
  compact. That holds even when more than half the entries are live. Otherwise do nothing.
- **Nothing shrinks** on removal.

Worked charges for `Map[u64, u64]`, where charge(c) = c × 32:

- 4 → 8 admits 256 and refunds 128.
- With all 4 slots used, remove two keys and insert an absent one. Live is 2, which is at most
  2, so the Map compacts. The two survivors keep their order, the new entry goes last, and
  nothing is charged.
- Remove only one key instead and insert. Live is 3, which is more than 2, so the Map grows to 8.

The block's physical size is
engine layout and is not charged (MEM-19, MEM-28):

| capacity | block bytes (S = 24, 80-byte groups) | logical charge |
|---|---|---|
| 1 (a one-entry clone) | 24 + 32 + 80 = 136 | 32 |
| 4 | 96 + 32 + 80 = 208 | 128 |
| 5 | 120 + 32 + 80 = 232 | 160 |
| 8 | 192 + 32 + 80 = 304 | 256 |

**Clone** ([MEM-17](../spec/memory.md#mem-17-clone)). Cloning a Map with live count 0 makes a Map with
cap 0 that admits nothing. It is strong-empty when the source is strong or strong-empty, and
ordinary otherwise.

Otherwise the clone allocates capacity = live, charging charge(live), and takes the source's
mode. Then it walks the source's live entries in order. For each, it clones the key and then the
value. `copy_entry` moves both fields into the next slot, copies the source's meta (valid
because the modes agree), and advances `used` and the live count. This makes the complete pair
available to cleanup before cloning the next one. After the last entry it rebuilds once.

A fast clone's rebuild may promote. Promotion re-places every entry under the strong word, so a
fast meta is never left in a strong Map. If a clone fails partway, the copy's cleanup cleans the
copied prefix in order and frees the block. The clone never probes the source, and it never
probes the copy before its rebuild.

**Cost.** Before promotion, every individual search visits at most 8 groups and compares at most
128 candidate keys, whatever the keys. Hits, misses and overwrites can keep taking the last group
of the window without promoting, and reusable DELETED buckets allow further fast insertions.
That is bounded work, not a small average. In strong mode a search has the expected cost of a
half-full table under a keyed PRF, for keys chosen without the run key. That is an expectation
over keys, not a bound on one fixed run.

**Private operations.** `core.map` reaches a block only through bodiless private functions. The
compiler lowers each inline from one layout definition, on every engine. The table gives each
function's semantics number (PROG-10, Builtins) in normal form 3:

| function | number | meaning |
|---|---|---|
| `place(k)` | 1 (new) | The fast placement word: engine operation `MapPlaceWord` (runtime tag 60) for the word class, and `MapPlaceBytes` (61) for the byte class; never lowered for a composite type. The word form is a pure function of k and the salts, and may be hoisted over the mode test. The byte form reads b[0..n) and the salts: it may be reused across Maps only while no write can reach those bytes, and is not computed eagerly in strong mode. Only its salt loads are invariant for the task. |
| `strong(k)` | 1 (new) | The strong placement word: `MapStrongWord` (62), `MapStrongBytes` (63), or for a composite key `MapStrongStart` (64) followed by the unchanged `MapHashAddWord` (44), `MapHashAddBytes` (45) and `MapHashFinish` (46). |
| `fast_class(m)` | 1 (new) | A constant from the Map's key type: 0 for composite, 1 for word, 2 for byte. This lets word placement precede the mode test while byte placement stays on the fast branch. |
| `mode(m)`, `group_mask(m)` | 1 (new) | The header's strong bit and group mask. Not called when cap is 0. |
| `strong_empty(m)`, `set_strong_empty(mut m)` | 1 (new) | Read and set the carrier's strong-empty mark (cap 0 only). |
| `group_match(m, g, t)` | 1 (new) | A 16-bit mask whose bit j is set when group g's byte j equals t. |
| `group_empty(m, g)`, `group_vacant(m, g)` | 1 (new) | The same mask for EMPTY bytes, and for EMPTY or DELETED bytes. |
| `bucket_entry(m, b)` | 1 (new) | Bucket b's entry index. |
| `install(mut m, b, p, i)` | 1 (new) | See **Writers**. |
| `erase(mut m, b)` | 1 (new) | Erase bucket b; called only by `remove`. |
| `clear_index(mut m)`, `set_strong(mut m)` | 1 (new) | Set every control byte to EMPTY; set the strong bit. |
| `slot_meta(m, i)` | 1 (new) | Slot i's meta. |
| `hash(k)` | 2 (kept) | `map.hash`, as in version 2. NF3's `core.map` does not call it. |
| `capacity(m)`, `slots(m)` | 1 (kept) | Carrier reads, unchanged. |
| `new` | 1 (kept) | Makes `{0, 0, 0}`, as in version 2. |
| `advance(mut m)` | 1 (new) | Add 1 to `used` and to the live count; called only by `commit`. |
| `compact`, `kill`, `len`, `regrow`, `slot_key`, `slot_value`, `swap_value`, `take_entry`, `with_value` | 2 | Same names, new block layout. |
| `push_entry(mut m, take k, take v)` | 2 | Move k and v into slot `used`; it no longer takes a hash, and does not change `used`. |
| `copy_entry(mut m, meta, take k, take v)` | 1 (new) | Clone's completed pair and copied meta, appended to the initialized prefix before rebuilding. |
| `try_regrow` | 2 | A first explicit entry. Under version 2 it had none, and so ran at 1. |

The functions `slot_hash`, `slot_live`, `index_len`, `index_get` and `index_set` are gone.
Source-defined `core.map` functions (`get`, `insert`, `probe_insert`, `place_entry`, `commit`,
`promote`, `rebuild`, `clone` and the rest) have bodies, and so take ordinary body hashes.

Each mask is exact on every target. SSE2, NEON and the portable word-at-a-time form give equal
masks, with no false positives, at lanes 0 and 15 and on either byte order. The probe result is a
core-private enum with the cases `Found(entry, bucket)`, `Vacant(bucket)` and `Crowded`, and it
carries p.

The runtime tags 60–64 are appended to the engine-operation table. Each engine lists them among
the operations it supports, so admission of a published NF3 body checks them like any other tag.
`MapPlaceWord` and `MapStrongWord` take a `u64`. `MapPlaceBytes` and `MapStrongBytes` take a
pointer and a length. Each of those four returns a `u64`. `MapStrongStart` initializes a hash
state, as `MapHashStart` does, from k_s.
