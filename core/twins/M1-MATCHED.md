# Corrected M1 matched Rust samples

Review930 accepted the nine workload algorithms and six provider limits, and withheld approval for the remaining source-work differences at76d9b6ce. The delta below corrects those findings. Final independent delta review is pending, so no matched ratio is a result yet. Hirsel programs, idiomatic twins and EXPECTED stay unchanged.

The final sources pass232/232 exact observations:58 applicable cases, dev/release, cold/warm, one sample per configuration. stdout, stderr, exit status and changed/deleted files match committed EXPECTED. All18 rustc dep-info closures match core/tools/measure.py:sources. Five targeted laws cover UTF-8 endpoints/scalars, search/decimal/error classes, map hash/live relocation, float rounding/long scanning, and Files label ownership. The previously qualified stdout-to-/dev/full control retains I/O-error precedence. Land15 counters are superseded and retained in git/evidence, never blended.

Normal warm Rust observations follow. Full source/binary hashes, rustc version/flags and reference artifact identities are in M1-MATCHED-ROWS.tsv; environment and per-program source hashes are in M1-MATCHED-ENVIRONMENT.json.

| Program | Dev instructions / RSS KiB | Release instructions / RSS KiB | Source closure SHA256 |
|---|---:|---:|---|
| bfs | 486,991,988 / 3584 | 77,946,025 / 3584 | 4ff3bfc6dbe6 |
| generics | 210,516,326 / 3840 | 52,027,523 / 3840 | 5e42ecf8b44f |
| lru | 250,207,306 / 2560 | 42,604,959 / 2560 | 2634901f2d1a |
| files | 109,619,999 / 3328 | 1,707,095 / 3072 | bfc9616d1c12 |
| json | 444,909,516 / 2560 | 52,435,469 / 2304 | 7cc22504e8c5 |
| pipeline | 181,622,437 / 3332 | 10,944,056 / 3072 | 056f74023781 |
| request-scratch | 178,857,440 / 2816 | 32,353,401 / 2816 | 0128771f88f4 |
| rope | 71,968,479 / 2816 | 14,695,669 / 2816 | 5c41b14d0500 |
| versions | 11,116,534 / 2304 | 3,008,838 / 2048 | ca6359f23451 |

Every row uses pinned rustc1.99.0 b940084d7, SHA256 f3834d26669b03f6855fa54bd2381443123e860167bc0c7a8dd137e5cf6e5e4f, x86-64, overflow checks, CGU1, no debug info/incremental/LTO. Dev is opt0 with debug assertions; release is opt3 without them. Source_base records ae00f549 plus the final forward edits; source_sha256 identifies the exact qualified closure committed with the rows.

The shared helper verified the frozen M2 reference pair, compiler release/2f1f30544428e06b3d89bef6ea21cde83d03db780949f339030307055aad9557 and runtime shipping-release-fat-lto/95dd816277b936c865ccba458657918917889d6cd86a109f8343c1f03f3c8bb7. These Rust-only rows execute neither reference artifact. Pair them only with905's Hirsel observations at e96338cf41d71efd2076a9902068048a37b742c8, after delta approval. Historical old-H/new-Rust ratios remain diagnostic.

| Finding | Rust delta against 76d9b6ce | Mirrored Hirsel site at frozen M2 e96338cf |
|---|---|---|
| N1 slice endpoints and call-site audit | `matched_storage.rs:128` checks range and both continuation-byte endpoints. Line/split/prefix use it; Generics rehash/probe/render, request header/arena slices and JSON token/write/path slices now call View::slice. Removed the char-pattern adapter; delimiters are borrowed str literals. Actual byte/list slices stay byte slices. | core.string.slice:101-111; m1.support.line:67; Generics matched:32,45,118; request matched:14-15,51-53,91; std.json token/write and path cuts. |
| N2 scalar decoding | `matched_storage.rs:638` next_scalar retains end, initial continuation, indexed tail reads and checked char conversion. `json/matched_json.rs:542` quote calls it before the existing scalar/hex encoder. | core.string.next_scalar:113-149; json/main:248. |
| M1 String hash | `matched_map.rs:126` adds mix(0, byte_hash), then h xor (h >> 32). The source abc hash and initial /alpha, /beta slots are laws. | key_glue.rs String fold/hash and mix; core.map key hash. |
| M2 map regrowth | `matched_map.rs:61` growth drops dead entries like compaction and preserves live insertion order. Capacity64 to128 leaves64 live slots after the next insertion, not65. | maps.rs map_relocate for both compact/regrow; core.map rebuild. |
| J2 float parser | `json/matched_float.rs:21,97,122,129` ports scan/accumulate/eight/long, leading-zero inspection and the second scan, with many=true for long tails. Lemire uses leading_zeros. Format is constructed/passed to decide/lemire/slow. Float/JSON variants retain offsets and checked start+p before M1 error mapping. | std.float:138-285,322,394-397 and Format flow; std.json number_size:397-426 and parse-error mapping. |
| S5 static labels, classes and Files I/O | `matched_support.rs:15,33` constructs class2 or explicit cancelled class4. Pipeline calls cancelled at143,155,181. `files/matched.rs:84` copies labels only at source io calls. Held.drop unlocks before close; ignored probe-unlock and explicit fallible Owned unlock follow source cleanup. | m1.support.error:9-10; pipeline matched:162,219,302-306; files io:67-70; std.file.Lock.drop:126-130 and files release. |
| F2 owned setup bytes | `files/matched.rs:153` creates exact-capacity eight-byte owned existing buffer, writes it and drops it at setup-block end. | files/main:145-147; owned byte-literal lowering. |
| F3 lock open | `files/matched.rs:81` uses append(true).create(true), without the extra read permission. | files/main:93,151,160; std.file Append and runtime O_WRONLY, O_CREAT, O_APPEND. |
| M2 library drift after the review target | `matched_storage.rs:362` keeps the two-word ASCII scan and ports direct two/three/four-byte UTF-8 branches from5b59b4b0. | core.string.from_utf8 at frozen e96338cf. No canonical Hirsel change. |

The six accepted safe-provider limits remain. The map uses two typed buffers and safe moved-from defaults instead of one heterogeneous allocation. Safe Read initializes each spare range once, totaling final capacity C, plus Files'4096-byte copy buffer; that adds O(C) stores and C can reach2N to discover EOF. File drop cannot report real close failures. Temporary-directory naming differs. The standard-channel cancellation adapter preserves scripted custody/results but omits Hirsel dispatch and has scheduler-dependent spin costs. Rust does not counterfeit root funding or physical OOM behavior. These costs can move H/R in either direction; the detailed limits and exclusions remain in review930 and M1-PARITY.md.

Reuse was checked in921/922: no search/decimal/dense-map ports existed to reuse, so M1 shares one port of each. Both previously reviewed introsorts remain unchanged. All library bodies follow frozen M2 core/std, including the current float and UTF-8 bodies. No unsafe, artificial work, black_box or inlining hints were introduced.

Reproduce using each row's direct pinned rustc flags and core/corpus/m1/<program>/cases.json, isolated case directories and unchanged EXPECTED. The lane's m1-parity/collect-930.py records exact commands. CPU16, process-tree instructions:u/duration_time, GNU time, tools/capped at2GiB, shared measurement lock and unflushed OS pages were used. No Hirsel measurement was repeated for this twin delta.
