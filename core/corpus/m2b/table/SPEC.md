# M2b table cases

Program: `core/programs/m2b/table/` (library module `m2b.table`, cases in `main.hirsel`).
Runner: `core/tools/m2b.py` (Buck `//:core-m2b`): evaluator through
`hirsel-core-contract-eval` on `case_<name>`, Cranelift and LLVM through the built
`main <name>`, which prints the same value. Every value is derived by hand below from
the codec layout and MEM-24 charges; none was pasted from a run. `used` is the bytes
still admitted when the case returns: every case frees what it made, so it is 0.

Codec layout: `id`, `balance` (two's complement), owner byte length, each a little-endian
u64, then the owner's UTF-8 bytes. One row is 24 + byte length bytes.

## roundtrip = 3078
Rows (1, "ann", 10), (2, "bob", -5000000000), (3, "", 0) are put, encoded in key order
into one buffer and decoded back in order; each decoded row is compared field by field
with the stored one. 3 matches; buffer 27 + 27 + 24 = 78 bytes; decoding ends at 78.
Value `matches × 1000 + length` = 3078. Catches: a sign or width error in the balance
round trip, an off-by-one in the owner slice, a cursor that does not advance exactly.

## malformed = 51
From the 27-byte encoding of (7, "ann", 10): empty input; the first 26 bytes (owner cut
short); the first 23 bytes (length field cut short); the length field set to 2^64 - 1;
byte 25 (the second owner byte) set to 0xff, invalid UTF-8. Each must decode to `None`
and leave the cursor at 0: 5. The intact encoding decodes to the row with the cursor at
27: 1. Value `5 × 10 + 1` = 51. Catches: the length check written `p + n > len`, which
fails with Overflow at 2^64 - 1 instead of refusing (the plan's probe had it), a cursor
moved by a refused decode, and an owner accepted without UTF-8 validation.

## capacity = 512256
`Account` is 8 + 24 + 8 = 40 bytes, so a `Map[u64, Account]` entry charges 40 + 8 + 16
= 64 (MEM-24). The table is empty, so its first growth is charged to the current limit
(MEM-26): inside `bound bytes 512`, rows 1..4 grow it to capacity 4, 256 bytes. The put
of row 5 needs capacity 8, 512 bytes, admitted before the old storage is refunded, with
512 - 256 = 256 available: refused, `Capacity { requested: 512, available: 256 }`. Then:
the row comes back intact (owner "zed", id 5); the table still has 4 rows, balances
10 + 20 + 30 + 40 = 100, and no row 5; after the bound the same put succeeds (the
storage's record now leads to the task's limit) and the table has 5 rows. Value
`requested × 1000 + available` = 512256; a broken check returns its number 1..7
instead. Catches: a put that installs or drops the row on a refused growth, and a
growth not charged where MEM-26 says.
