# Benchmark inventory

Source inspection at main `80aad219`, 2026-11-09. "Not rerun" means current
executability is unknown. An existing runner or historical report is not fresh
passing evidence. Only the eight fixed-corpus rows are built and run in this
lane. Their executed results appear in the baseline report.

| Workload | Rust twin | Algorithm, input and output equivalence | Current run |
|---|---|---|---|
| micro/bigint_mul | None dedicated | Unqualified. | Not rerun |
| micro/channel | micro/rust/main.rs | Same generated input, collection/loop algorithm and checksum. Rust std thread/mpsc replaces Hirsel cooperative task/channels. | Fixed corpus |
| micro/gzip_read | New runtime-corpus/gzip.rs | Same embedded gzip, streaming RFC1951 inflate, CRC validation, 4096-byte reads, length and full byte sum. | Fixed corpus |
| micro/html_row | micro/rust/main.rs | Same generated input, collection/loop algorithm and checksum. | Not rerun |
| micro/json | micro-json-reference/main.rs | Same document, ten DOM parse/stringify passes, serialized length; key order ignored. | Fixed corpus |
| micro/json_scoped | None dedicated | Unqualified. | Not rerun |
| micro/json_scoped_check | None dedicated | Unqualified. | Not rerun |
| micro/json_typed | micro-json-reference/typed.rs | Same direct typed decoding and row checksum. | Not rerun |
| micro/json_typed_dom | Typed reference exists | Same input/checksum, different algorithm route: Rust direct decode versus Hirsel DOM-to-record. Not a DOM cost twin. | Not rerun |
| micro/map_str_upsert | micro/rust/main.rs | Same generated input, collection/loop algorithm and checksum. | Not rerun |
| micro/float_math | micro/rust/main.rs | Same generated input, collection/loop algorithm and checksum. | Fixed corpus |
| micro/int_bits | micro/rust/main.rs | Same generated input, collection/loop algorithm and checksum. | Not rerun |
| micro/map_remove | micro/rust/main.rs | Same generated input, collection/loop algorithm and checksum. | Not rerun |
| micro/map_str | micro/rust/main.rs | Same generated input, collection/loop algorithm and checksum. | Not rerun |
| micro/map_u64 | micro/rust/main.rs | Same generated input, collection/loop algorithm and checksum. | Fixed corpus |
| micro/print | micro/rust/main.rs | Same generated input, collection/loop algorithm and checksum. | Not rerun |
| micro/sha | micro/rust/main.rs | Same generated input, collection/loop algorithm and checksum. | Not rerun |
| micro/sort | micro/rust/main.rs | Same generated input, collection/loop algorithm and checksum. | Not rerun |
| micro/strbuild | micro/rust/main.rs | Same generated input, collection/loop algorithm and checksum. | Fixed corpus |
| micro/map_str_entry, map_str_two_probe | micro/rust/main.rs | Rust-only probe variants; no separately named Hirsel source. | Not rerun |
| micro/http keep-alive, hello | std TCP and Tokio/httparse | Same finite requests, response bodies and count; std parser handles a narrower fixed grammar. | Not rerun |
| micro/net net-only | micro/http/server.rs | Same request/response bytes, but Hirsel omits HTTP parsing; cost floor only. | Not rerun |
| micro-json-reference dynamic, typed | Two Rust reference targets | References for JSON micros above; not extra workloads. | Dynamic in corpus; typed not rerun |
| flate encode levels 1/6/9, words/noise/runs | None; C zlib control | C uses matching zlib algorithms, fixtures and compressed-byte CRC. | Not rerun |
| zstd decode | zstd/decode.rs | Same input, ten whole-buffer decodes, XXH64/length/edge checksum. Unsafe libzstd FFI excludes it from safe corpus. | Not rerun |
| numeric Toom-3 crossover | None independent | Hirsel-only bigint crossover variants. | Not rerun |
| priority-queue-combined push_pop, replace_top | priority-queue-combined/control.rs | Same LCG, min-heap ordering/ties, capacity, returned values and complete drain checksum. Explicit push/pop controls also exist. | push_pop in corpus; other modes not rerun |
| store load, compact, recompact, begin | None | Hirsel before/after snapshot-size and work comparison. | Not rerun |
| apps/tasks cold, restart, search, create, update, SSE/RSS/replay | apps/tasks/rust | Matched application/storage contracts and differential HTTP/persistence verification. Parser diagnostics can differ. | Not rerun |
| langarena/Base64::Encode | LangArena Rust at 3c9f84d | Pinned upstream algorithm port, same generated inputs and test.js expected checksums. Group source comments cite Rust files/lines. | Not rerun |
| langarena/Base64::Decode | LangArena Rust at 3c9f84d | Pinned upstream algorithm port, same generated inputs and test.js expected checksums. Group source comments cite Rust files/lines. | Not rerun |
| langarena/Binarytrees::Obj | LangArena Rust at 3c9f84d | Pinned upstream algorithm port, same generated inputs and test.js expected checksums. Group source comments cite Rust files/lines. | Not rerun |
| langarena/Binarytrees::Arena | LangArena Rust at 3c9f84d | Pinned upstream algorithm port, same generated inputs and test.js expected checksums. Group source comments cite Rust files/lines. | Not rerun |
| langarena/Brainfuck::Array | LangArena Rust at 3c9f84d | Pinned upstream algorithm port, same generated inputs and test.js expected checksums. Group source comments cite Rust files/lines. | Not rerun |
| langarena/Brainfuck::Recursion | LangArena Rust at 3c9f84d | Pinned upstream algorithm port, same generated inputs and test.js expected checksums. Group source comments cite Rust files/lines. | Not rerun |
| langarena/Etc::CacheSimulation | LangArena Rust at 3c9f84d | Pinned upstream algorithm port, same generated inputs and test.js expected checksums. Group source comments cite Rust files/lines. | Not rerun |
| langarena/Calculator::Ast | LangArena Rust at 3c9f84d | Pinned upstream algorithm port, same generated inputs and test.js expected checksums. Group source comments cite Rust files/lines. | Not rerun |
| langarena/Calculator::Interpreter | LangArena Rust at 3c9f84d | Pinned upstream algorithm port, same generated inputs and test.js expected checksums. Group source comments cite Rust files/lines. | Not rerun |
| langarena/CLBG::Fannkuchredux | LangArena Rust at 3c9f84d | Pinned upstream algorithm port, same generated inputs and test.js expected checksums. Group source comments cite Rust files/lines. | Not rerun |
| langarena/CLBG::Mandelbrot | LangArena Rust at 3c9f84d | Pinned upstream algorithm port, same generated inputs and test.js expected checksums. Group source comments cite Rust files/lines. | Not rerun |
| langarena/CLBG::Nbody | LangArena Rust at 3c9f84d | Pinned upstream algorithm port, same generated inputs and test.js expected checksums. Group source comments cite Rust files/lines. | Not rerun |
| langarena/CLBG::Spectralnorm | LangArena Rust at 3c9f84d | Pinned upstream algorithm port, same generated inputs and test.js expected checksums. Group source comments cite Rust files/lines. | Not rerun |
| langarena/Compress::BWTEncode | LangArena Rust at 3c9f84d | Pinned upstream algorithm port, same generated inputs and test.js expected checksums. Group source comments cite Rust files/lines. | Not rerun |
| langarena/Compress::BWTDecode | LangArena Rust at 3c9f84d | Pinned upstream algorithm port, same generated inputs and test.js expected checksums. Group source comments cite Rust files/lines. | Not rerun |
| langarena/Compress::HuffEncode | LangArena Rust at 3c9f84d | Pinned upstream algorithm port, same generated inputs and test.js expected checksums. Group source comments cite Rust files/lines. | Not rerun |
| langarena/Compress::HuffDecode | LangArena Rust at 3c9f84d | Pinned upstream algorithm port, same generated inputs and test.js expected checksums. Group source comments cite Rust files/lines. | Not rerun |
| langarena/Compress::ArithEncode | LangArena Rust at 3c9f84d | Pinned upstream algorithm port, same generated inputs and test.js expected checksums. Group source comments cite Rust files/lines. | Not rerun |
| langarena/Compress::ArithDecode | LangArena Rust at 3c9f84d | Pinned upstream algorithm port, same generated inputs and test.js expected checksums. Group source comments cite Rust files/lines. | Not rerun |
| langarena/Compress::LZWEncode | LangArena Rust at 3c9f84d | Pinned upstream algorithm port, same generated inputs and test.js expected checksums. Group source comments cite Rust files/lines. | Not rerun |
| langarena/Compress::LZWDecode | LangArena Rust at 3c9f84d | Pinned upstream algorithm port, same generated inputs and test.js expected checksums. Group source comments cite Rust files/lines. | Not rerun |
| langarena/CSV::Parse | LangArena Rust at 3c9f84d | Pinned upstream algorithm port, same generated inputs and test.js expected checksums. Group source comments cite Rust files/lines. | Not rerun |
| langarena/Distance::Jaro | LangArena Rust at 3c9f84d | Pinned upstream algorithm port, same generated inputs and test.js expected checksums. Group source comments cite Rust files/lines. | Not rerun |
| langarena/Distance::NGram | LangArena Rust at 3c9f84d | Pinned upstream algorithm port, same generated inputs and test.js expected checksums. Group source comments cite Rust files/lines. | Not rerun |
| langarena/Etc::GameOfLife | LangArena Rust at 3c9f84d | Pinned upstream algorithm port, same generated inputs and test.js expected checksums. Group source comments cite Rust files/lines. | Not rerun |
| langarena/Graph::BFS | LangArena Rust at 3c9f84d | Pinned upstream algorithm port, same generated inputs and test.js expected checksums. Group source comments cite Rust files/lines. | Not rerun |
| langarena/Graph::DFS | LangArena Rust at 3c9f84d | Pinned upstream algorithm port, same generated inputs and test.js expected checksums. Group source comments cite Rust files/lines. | Not rerun |
| langarena/Graph::AStar | LangArena Rust at 3c9f84d | Pinned upstream algorithm port, same generated inputs and test.js expected checksums. Group source comments cite Rust files/lines. | Not rerun |
| langarena/Hash::SHA256 | LangArena Rust at 3c9f84d | Pinned upstream algorithm port, same generated inputs and test.js expected checksums. Group source comments cite Rust files/lines. | Not rerun |
| langarena/Hash::CRC32 | LangArena Rust at 3c9f84d | Pinned upstream algorithm port, same generated inputs and test.js expected checksums. Group source comments cite Rust files/lines. | Not rerun |
| langarena/Json::Generate | LangArena Rust at 3c9f84d | Pinned upstream algorithm port, same generated inputs and test.js expected checksums. Group source comments cite Rust files/lines. | Not rerun |
| langarena/Json::ParseDom | LangArena Rust at 3c9f84d | Pinned upstream algorithm port, same generated inputs and test.js expected checksums. Group source comments cite Rust files/lines. | Not rerun |
| langarena/Json::ParseMapping | LangArena Rust at 3c9f84d | Pinned upstream algorithm port, same generated inputs and test.js expected checksums. Group source comments cite Rust files/lines. | Not rerun |
| langarena/Etc::LogParser | LangArena Rust at 3c9f84d | Same 13 regex patterns, generated log, scans, count map and total; standalone exact stdout check. | Fixed corpus adapter |
| langarena/Matmul::Single | LangArena Rust at 3c9f84d | Pinned upstream algorithm port, same generated inputs and test.js expected checksums. Group source comments cite Rust files/lines. | Not rerun |
| langarena/Matmul::T4 | LangArena Rust at 3c9f84d | Pinned upstream algorithm port, same generated inputs and test.js expected checksums. Group source comments cite Rust files/lines. CPU-pool admission required. | Not rerun |
| langarena/Matmul::T8 | LangArena Rust at 3c9f84d | Pinned upstream algorithm port, same generated inputs and test.js expected checksums. Group source comments cite Rust files/lines. CPU-pool admission required. | Not rerun |
| langarena/Matmul::T16 | LangArena Rust at 3c9f84d | Pinned upstream algorithm port, same generated inputs and test.js expected checksums. Group source comments cite Rust files/lines. CPU-pool admission required. | Not rerun |
| langarena/Maze::Generator | LangArena Rust at 3c9f84d | Pinned upstream algorithm port, same generated inputs and test.js expected checksums. Group source comments cite Rust files/lines. | Not rerun |
| langarena/Maze::BFS | LangArena Rust at 3c9f84d | Pinned upstream algorithm port, same generated inputs and test.js expected checksums. Group source comments cite Rust files/lines. | Not rerun |
| langarena/Maze::AStar | LangArena Rust at 3c9f84d | Pinned upstream algorithm port, same generated inputs and test.js expected checksums. Group source comments cite Rust files/lines. | Not rerun |
| langarena/Etc::NeuralNet | LangArena Rust at 3c9f84d | Pinned upstream algorithm port, same generated inputs and test.js expected checksums. Group source comments cite Rust files/lines. | Not rerun |
| langarena/Etc::Sieve | LangArena Rust at 3c9f84d | Pinned upstream algorithm port, same generated inputs and test.js expected checksums. Group source comments cite Rust files/lines. | Not rerun |
| langarena/Sort::Quick | LangArena Rust at 3c9f84d | Pinned upstream algorithm port, same generated inputs and test.js expected checksums. Group source comments cite Rust files/lines. | Not rerun |
| langarena/Sort::Merge | LangArena Rust at 3c9f84d | Pinned upstream algorithm port, same generated inputs and test.js expected checksums. Group source comments cite Rust files/lines. | Not rerun |
| langarena/Sort::Self | LangArena Rust at 3c9f84d | Pinned upstream algorithm port, same generated inputs and test.js expected checksums. Group source comments cite Rust files/lines. | Not rerun |
| langarena/Template::Parse | LangArena Rust at 3c9f84d | Pinned upstream algorithm port, same generated inputs and test.js expected checksums. Group source comments cite Rust files/lines. | Not rerun |
| langarena/Template::Regex | LangArena Rust at 3c9f84d | Pinned upstream algorithm port, same generated inputs and test.js expected checksums. Group source comments cite Rust files/lines. | Not rerun |
| langarena/Etc::TextRaytracer | LangArena Rust at 3c9f84d | Pinned upstream algorithm port, same generated inputs and test.js expected checksums. Group source comments cite Rust files/lines. | Not rerun |
| langarena/Etc::Words | LangArena Rust at 3c9f84d | Pinned upstream algorithm port, same generated inputs and test.js expected checksums. Group source comments cite Rust files/lines. | Not rerun |
| native/batch-validation small/medium/large | native/rust/src/main.rs | Matched algorithm and owned/shared storage contract, fixed input and independent full-output oracle. See native/README.md. | Not rerun |
| native/binarytrees small/medium/large | native/rust/src/main.rs | Matched algorithm and owned/shared storage contract, fixed input and independent full-output oracle. See native/README.md. | Not rerun |
| native/closure-iter/erased small/medium/large | native/rust/src/main.rs | Matched algorithm and owned/shared storage contract, fixed input and independent full-output oracle. See native/README.md. | Not rerun |
| native/closure-iter/scoped small/medium/large | native/rust/src/main.rs | Matched algorithm and owned/shared storage contract, fixed input and independent full-output oracle. See native/README.md. | Not rerun |
| native/expr-compiler small/medium/large | native/rust/src/main.rs | Matched algorithm and owned/shared storage contract, fixed input and independent full-output oracle. See native/README.md. | Not rerun |
| native/json-roundtrip small/medium/large | native/rust/src/main.rs | Matched algorithm and owned/shared storage contract, fixed input and independent full-output oracle. See native/README.md. | Not rerun |
| native/persistent-list-map/shared small/medium/large | native/rust/src/main.rs | Matched algorithm and owned/shared storage contract, fixed input and independent full-output oracle. See native/README.md. | Not rerun |
| native/persistent-list-map/unique small/medium/large | native/rust/src/main.rs | Matched algorithm and owned/shared storage contract, fixed input and independent full-output oracle. See native/README.md. | Not rerun |
| native/records-messages/flat small/medium/large | native/rust/src/main.rs | Matched algorithm and owned/shared storage contract, fixed input and independent full-output oracle. See native/README.md. | Not rerun |
| native/records-messages/scalar small/medium/large | native/rust/src/main.rs | Matched algorithm and owned/shared storage contract, fixed input and independent full-output oracle. See native/README.md. | Not rerun |
| native/resource-failure/heavy small/medium/large | native/rust/src/main.rs | Matched algorithm and owned/shared storage contract, fixed input and independent full-output oracle. See native/README.md. | Not rerun |
| native/resource-failure/throughput small/medium/large | native/rust/src/main.rs | Matched algorithm and owned/shared storage contract, fixed input and independent full-output oracle. See native/README.md. | Not rerun |
| native/saxpy-checked/f32 small/medium/large | native/rust/src/main.rs | Matched algorithm and owned/shared storage contract, fixed input and independent full-output oracle. See native/README.md. | Not rerun |
| native/saxpy-checked/f64 small/medium/large | native/rust/src/main.rs | Matched algorithm and owned/shared storage contract, fixed input and independent full-output oracle. See native/README.md. | Not rerun |
| native/saxpy-checked/integer small/medium/large | native/rust/src/main.rs | Matched algorithm and owned/shared storage contract, fixed input and independent full-output oracle. See native/README.md. | Not rerun |
| native/slots-churn small/medium/large | native/rust/src/main.rs | Matched algorithm and owned/shared storage contract, fixed input and independent full-output oracle. See native/README.md. | Not rerun |
| native/wordfreq small/medium/large | native/rust/src/main.rs | Matched algorithm and owned/shared storage contract, fixed input and independent full-output oracle. See native/README.md. | Not rerun |
| native/binarytrees-region | Rust bumpalo arena variant | Same tree schedule/checksum, arena lifetime; separate from heap. | Not rerun |
| native/wordfreq-owned-bulk | Existing wordfreq Rust control | Same tokens/counts/output; bulk copying changes Hirsel implementation only. | Not rerun |
| native/resource-drops drafts and correctness control | Rust Drop draft | Effectful destruction timing explicitly unresolved. Not a qualified timing pair. | Not rerun |
| native/startup serial, pool | None | Hirsel CPU-pool startup controls. | Not rerun |
| multicore/yield | multicore/rust/src/main.rs, Tokio | Matched logical work/checksum; width, provider and recording admission are separate. See multicore/README.md. | Not rerun |
| multicore/cancel_depth | multicore/rust/src/main.rs, Tokio | Matched logical work/checksum; width, provider and recording admission are separate. See multicore/README.md. | Not rerun |
| multicore/atomic | multicore/rust/src/main.rs, Tokio | Matched logical work/checksum; width, provider and recording admission are separate. See multicore/README.md. | Not rerun |
| multicore/roots | multicore/rust/src/main.rs, Tokio | Matched logical work/checksum; width, provider and recording admission are separate. See multicore/README.md. | Not rerun |
| multicore/mutex | multicore/rust/src/main.rs, Tokio | Matched logical work/checksum; width, provider and recording admission are separate. See multicore/README.md. | Not rerun |
| multicore/allocation | multicore/rust/src/main.rs, Tokio | Matched logical work/checksum; width, provider and recording admission are separate. See multicore/README.md. | Not rerun |
| multicore/independent_files | multicore/rust/src/main.rs, Tokio | Matched logical work/checksum; width, provider and recording admission are separate. See multicore/README.md. | Not rerun |
| multicore/namespace_mix | multicore/rust/src/main.rs, Tokio | Matched logical work/checksum; width, provider and recording admission are separate. See multicore/README.md. | Not rerun |
| multicore/fork_join | multicore/rust/src/main.rs, Tokio | Matched logical work/checksum; width, provider and recording admission are separate. See multicore/README.md. | Not rerun |
| multicore/late_set | multicore/rust/src/main.rs, Tokio | Matched logical work/checksum; width, provider and recording admission are separate. See multicore/README.md. | Not rerun |
| multicore/channel | multicore/rust/src/main.rs, Tokio | Matched logical work/checksum; width, provider and recording admission are separate. See multicore/README.md. | Not rerun |
| multicore/extern | multicore/rust/src/main.rs, Tokio | Matched logical work/checksum; width, provider and recording admission are separate. See multicore/README.md. | Not rerun |
| tasks-perf/casefold | None isolated | Hirsel before/after micro; the Tasks Rust app is not an isolated twin. | Not rerun |
| tasks-perf/contains | None isolated | Hirsel before/after micro; the Tasks Rust app is not an isolated twin. | Not rerun |
| tasks-perf/decimal | None isolated | Hirsel before/after micro; the Tasks Rust app is not an isolated twin. | Not rerun |
| tasks-perf/html | None isolated | Hirsel before/after micro; the Tasks Rust app is not an isolated twin. | Not rerun |
| tasks-perf/html_plain | None isolated | Hirsel before/after micro; the Tasks Rust app is not an isolated twin. | Not rerun |
| tasks-perf/json_parse | None isolated | Hirsel before/after micro; the Tasks Rust app is not an isolated twin. | Not rerun |
| tasks-perf/json_record | None isolated | Hirsel before/after micro; the Tasks Rust app is not an isolated twin. | Not rerun |
| tasks-perf/json_record_dom | None isolated | Hirsel before/after micro; the Tasks Rust app is not an isolated twin. | Not rerun |
| tasks-perf/normalize | None isolated | Hirsel before/after micro; the Tasks Rust app is not an isolated twin. | Not rerun |
| tasks-perf/search | None isolated | Hirsel before/after micro; the Tasks Rust app is not an isolated twin. | Not rerun |
| tasks-perf/slice_copy | None isolated | Hirsel before/after micro; the Tasks Rust app is not an isolated twin. | Not rerun |
