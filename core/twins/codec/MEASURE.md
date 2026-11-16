# std.codec against a hand-written Rust codec of the same format

Sources:
- Hirsel side: `core/twins/codec/codec.hirsel`, which uses `std.codec`. Its codecs are the code the derive generates: `make` with put/take functions per field and `put_list`/`take_list` over a codec.
- Rust twin: `core/twins/codec/codec.rs`, the same bytes through a `Codec` trait (monomorphised, `Vec::extend_from_slice`), with the same decode checks (bounds, the count against the bytes left, UTF-8, canonical NaN).

`codec WORK MODE N R`: WORK 0 is N rows `{id: u64, owner: String, balance: i64}`, 1 is N f64s, 2 is N strings, each one List; MODE 0 builds and encodes once, 1 also encodes R times, 2 also decodes R times. Both sides print the same checksum.

Method:
- The release compiler (sha256 a411c956…) and the shipping fat-LTO runtime (sha256 8ac12360…) are pinned by `core/tools/measurement_artifacts.py`.
- Hirsel is measured optimised (LLVM `--release`) and in development (Cranelift dev); Rust with the workspace profiles (opt 3 and opt 0, overflow checks on, CGU 1).
- Runs are pinned to CPU 16 while holding /tmp/hirsel-langarena-measure.lock; `perf stat -e instructions:u` over the whole process, median of 20 samples; wall is the median of 5 unprofiled runs.
- A pass = (MODE m − MODE 0) / R at N=100,000, R=20. Per element = a pass / N.
- Script: /workspace/code/hirsel-briefs/std-codec-1008/tools/measure2.py; raw samples in /tmp/m4.json (2026-11-16, with put_string reserving once).

| one pass, N=100,000 | Hirsel LLVM / Rust opt 3, instructions | wall | Cranelift dev / Rust opt 0, instructions | wall |
|---|---:|---:|---:|---:|
| rows encode | 14.4M / 5.3M = 2.72 (144 / 53 per row) | 0.99 / 0.63 ms = 1.59 | 0.30 | 0.36 |
| rows decode | 66.8M / 55.6M = 1.20 | 5.41 / 3.43 ms = 1.58 | 0.94 | 1.09 |
| f64 encode | 1.8M / 1.3M = 1.38 | 0.08 / 0.06 ms = 1.32 | 0.36 | 0.43 |
| f64 decode | 2.1M / 1.7M = 1.24 | 0.13 / 0.10 ms = 1.23 | 0.83 | 1.13 |
| strings encode | 8.0M / 3.7M = 2.16 (80 / 37 per string) | 0.62 / 0.28 ms = 2.22 | 0.30 | 0.32 |
| strings decode | 59.9M / 53.9M = 1.11 | 6.95 / 5.58 ms = 1.25 | 1.04 | 1.00 |

## Where encode's excess goes (rows: 91 instructions a row over Rust)

- The stored functions cost nothing. `put_list` is monomorphised per codec and `accounts()`/`of_string()` fold, so each element is a direct `call` to `account_encode` and from it to `put_string`; no indirect call remains (unlike Tally's stored combine).
- About 60 are two call boundaries per row, `account_encode` and `put_string`, which LLVM does not inline. Each pays six pushes and pops, a 0x98–0xa8 byte frame with spills, the 24-byte `Result[(), Capacity]` written through memory, and its tag tested twice by the caller (`test %al` and `cmpb $1, off(%rsp)`). They stay out of line because the front expands `list.put_word_le` and `list.append` with the whole reserve and growth body inline (`hirsel_core_grow`, `alloc_ordinary`, two `memcpy`, `free` at every put; `crates/hirsel-core-front/src/body/lists.rs` `list_put_word_le` calls `list_reserve`), where `push` takes a cold call to `list.grow`. `put_string` is 669 instructions and the row encoder about 1,500. That growth body is core-llvm-905's subject.
- Capacity tests are no net share. Each put tests `len + 8` for overflow and against capacity (5 instructions) where Rust's `extend_from_slice` tests once (2), about 9 a row. One reservation ahead of the row (`list.reserve(mut out, 24 + byte_len(owner))`, what a derive would emit) costs more than it saves: 158 a row against 144. A guarded out-of-line reservation in `put_string` gave 87 against 80 for strings.
- The rest, about 20, is the loop's per-element Result handling and the stores of `len` back to the header after each put.
- Strings encode (43 over): one call boundary (`put_string`) and the same per-put tests.

`put_string` reserved `8 + len` before `put_word_le` and `append` each reserved again; it now reserves once, worth 13 instructions a row or string (157→144, 93→80).

## Decode

Inlined into one loop per list; the excess over Rust is the reload of `b`'s data pointer from the stack each element (register pressure in the large function) and `push`'s capacity test, which Rust also has. Rows decode wall (1.58) exceeds its instruction ratio (1.20): the 100,000 strings each allocate, and from_utf8 is 6.9 percent and malloc and free about 40 percent of a strings decode profile.
