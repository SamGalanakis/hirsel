# Corrected M1 matched Rust samples

Review932 approved eight twins WITH LIMITS at3d3ceedb and withheld JSON approval for D1/D2. This delta fixes D1/D2 and implements the orchestrator's R1 ruling on read initialization. The orchestrator will review only 3d3ceedb..this change. No new review lane is requested and no ratios are published here. Hirsel programs, idiomatic twins and EXPECTED remain unchanged; no Hirsel sample was taken.

All232 new Rust observations match EXPECTED:58 applicable cases, both opt levels, cold/warm, one sample each. Shared read_bytes serves every program, so R1 requires all232 rows; JSON also has D1/D2 and Files uses R1 for its copy buffer and both whole-file reads. stdout, stderr, exit status and changed/deleted files match. All18 rustc dep-info closures match core/tools/measure.py:sources. The existing JSON contract test now checks invalid escape and trailing-backslash offsets; it failed before D2 and passes after it. The previous232 observations at3d3ceedb are superseded, retained in git and rust-930-qualified evidence, never blended.

Normal warm Rust observations follow. M1-MATCHED-ROWS.tsv carries all232 absolute instruction/RSS observations, complete flags, compiler/binary/source hashes, review status and the M2 reference artifact identity in every row. M1-MATCHED-ENVIRONMENT.json has the environment and per-program source hashes.

| Program | Dev instructions / RSS KiB | Release instructions / RSS KiB | Source closure SHA256 |
|---|---:|---:|---|
| bfs | 449,361,548 / 3584 | 77,206,125 / 3584 | 03ee632ee359 |
| generics | 205,811,872 / 3840 | 52,011,694 / 3840 | e5177f5aca61 |
| lru | 231,342,665 / 2560 | 42,421,896 / 2304 | fca0ea052a2e |
| files | 71,991,839 / 3328 | 1,717,071 / 3072 | 0ab22f2ce76a |
| json | 435,397,290 / 2560 | 52,379,008 / 2304 | ecfecd0af691 |
| pipeline | 143,984,846 / 3332 | 10,934,423 / 3068 | c28e1a57ff3f |
| request-scratch | 160,045,653 / 2816 | 32,338,246 / 2812 | 6dbd71c918ce |
| rope | 67,263,898 / 2816 | 14,696,329 / 2560 | 8157655d1e0e |
| versions | 10,968,758 / 2304 | 3,008,797 / 2048 | 6b3dd1c35ee0 |

| Finding | Rust correction | Source rule or approved limitation |
|---|---|---|
| D1 | json/matched_float.rs:190 constructs Format immediately before decide, after scan and the exact-fast-path return. | Frozen M2 std.float.parse64:31-49. Opt0 disassembly calls scan at42883 and reaches format64 at42957 only on the slow path. |
| D2 | json/matched_json.rs:105 constructs Syntax(self.i+1) for an invalid escape, including a trailing backslash. Overflow checks retain the written addition. The existing JSON test asserts offset2 for both inputs. | Frozen M2 std.json:389-390; a backslash at index1 identifies the following position2. M1's final mapper discards this offset, so EXPECTED alone did not expose the bug. |
| R1 | matched_support.rs:41-48 appends up to4096 bytes at a time from one static zero slice, stopping at the pre-reserved capacity. read_bytes uses it after the unchanged reserve point; Files'4096-byte copy buffer uses the same initializer after its exact allocation. | Orchestrator ruling based on review932's closer safe form. Same allocation/growth points, live prefixes, initialized-byte totals and read/close/UTF-8 order. Opt0 append_elements calls memcpy at32fef; opt3 folds zero-chunk copies to bulk memset in main/run. The initialized-read provider limit remains. |

Pinned rustc1.99.0 b940084d7, SHA256 f3834d26669b03f6855fa54bd2381443123e860167bc0c7a8dd137e5cf6e5e4f, x86-64, overflow checks, CGU1, no debug info/incremental/LTO. Dev is opt0 with debug assertions; release is opt3 without them. Source_base is3d3ceedb plus this forward delta; source_sha256 identifies each exact qualified closure committed with the rows. Collector commands and raw observations are in the lane brief's m1-parity/collect-932.py and rust-932-qualified. CPU16, process-tree instructions:u/duration_time, GNU time, tools/capped at2GiB and the shared measurement lock are unchanged.

The shared-helper reference pair remains frozen M2 e96338cf41d71efd2076a9902068048a37b742c8: release compiler2f1f30544428e06b3d89bef6ea21cde83d03db780949f339030307055aad9557 and shipping-release-fat-lto runtime95dd816277b936c865ccba458657918917889d6cd86a109f8343c1f03f3c8bb7. These Rust-only samples execute neither artifact. Pairing and ratio publication belong to889 after the orchestrator approves this delta.

The six accepted provider limits remain: two typed map buffers with safe moved-from defaults; initialized Read storage; real close-error observability; temporary-directory naming; standard-channel cancellation/scheduler costs; root funding and physical OOM. Read initialization still writes final capacity C once per file, plus the4096-byte Files buffer; it is O(C), and exact-full EOF may require C=2N. The normal Files input and destination each initialize524288 bytes, totaling1048576, in addition to the copy buffer and required live-byte growth copies. R1 replaces avoidable per-byte opt0 helper work with bulk initialization, without changing that extra work count. Existing provider disclosures and accepted workload algorithms are otherwise unchanged. The prior Files dev observation is superseded and must not be described as an unqualified file-copy/code-generation advantage.
