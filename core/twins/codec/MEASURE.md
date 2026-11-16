# std.codec against Rust

String, Bytes, List and Map lengths and counts are four-byte little-endian u32s.
Value schemas use version 03 and key schemas version 04, including their u32 counts,
name lengths and definition indices. Counts above u32::MAX return an error.

The twins encode the same bytes and perform the same decode checks: bounds, count
against remaining bytes, UTF-8 and canonical NaN. Rust uses checked u32 conversion
and returns an encoding error; Hirsel returns Capacity.

`codec WORK MODE N R` selects rows `{id: u64, owner: String, balance: i64}` through
`of[List[Account]]` (0), f64s (1), strings (2), or rows through a derived record field
(3). Mode 0 constructs and encodes once; mode 1 adds R encodes; mode 2 adds R decodes.
A pass is `(mode - setup) / R`. Both sides' checksums agree in every measured cell.

Measured 2026-11-16, codec-u32-1046: one sample per cell at N=100,000, R=20, CPU 16,
under tools/capped with a 2 GiB process cap and perf instructions:u, one process at
a time under the shared measurement lock. Rust uses opt-level 3 or 0, checked
overflow and one codegen unit; Hirsel uses LLVM release or Cranelift development.
The release compiler and shipping fat-LTO runtime are pinned with
core/tools/measurement_artifacts.py. Baseline source is 0a25439f; the candidate
changes the wire counts and the derive's minimum sizes. The runtime is unchanged.
Ratios are Hirsel instructions divided by Rust instructions. This is one paired
sample, with no wall-time claim.

| one pass | LLVM before, H / R in millions = ratio | LLVM after, H / R in millions = ratio | dev ratio before to after |
|---|---:|---:|---:|
| rows via of encode | 14.60 / 5.30 = 2.755 | 20.00 / 5.80 = 3.448 | 0.298 to 0.608 |
| rows via of decode | 67.24 / 55.64 = 1.208 | 66.74 / 55.74 = 1.197 | 0.939 to 1.023 |
| f64 encode | 1.80 / 1.30 = 1.385 | 1.80 / 1.30 = 1.385 | 0.360 to 0.342 |
| f64 decode | 2.20 / 1.70 = 1.294 | 2.60 / 1.70 = 1.529 | 0.828 to 0.828 |
| strings encode | 8.00 / 3.70 = 2.162 | 13.60 / 4.10 = 3.317 | 0.301 to 0.921 |
| strings decode | 59.85 / 53.55 = 1.118 | 59.35 / 53.75 = 1.104 | 1.045 to 1.159 |
| rows via derived field encode | 14.60 / 5.30 = 2.755 | 20.00 / 5.80 = 3.448 | 0.298 to 0.608 |
| rows via derived field decode | 67.24 / 55.64 = 1.208 | 66.74 / 55.74 = 1.197 | 0.939 to 1.023 |

Row and string encoding regress: the u32 writer uses put_u32's four byte writes,
where the former count used list.put_word_le's bulk eight-byte write. The four-byte
String and collection minimums also reduce derive and decoder bounds. Rows and
strings decode slightly improve in LLVM; f64 decode regresses. These costs remain
visible; no gate ceiling is raised. Before this change the encode gate's ceilings
were rows 2.80 and strings 2.20; this sample exceeds both.

`core/tools/codec_encode.py --builder <hirsel> --runtime <release archive>
--out <dir> --check` gates the LLVM row and string encode ratios at 2.80 and 2.20.
The script defaults to five samples; this lane follows the host's one-sample rule
and reports the already measured ratios against these unchanged ceilings.
