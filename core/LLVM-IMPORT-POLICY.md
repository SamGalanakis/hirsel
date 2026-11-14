Current bound:1536 raw instructions after SSA-home emission. The former2048 bound was calibrated before that representation change. The retained boundary body std.float.scan changes1758 to1384 instructions; the first excluded body std.json.write_string changes2167 to1731. Max retained1384 is below min excluded1731, so1536 preserves all208/212 prior selections in the qualified M1/Arena closure. The same per-function bound runs after composition and remains far above LLVM's250 weighted O3 inline threshold.

The original pre-SSA qualification follows.

| Kind | Row | Control user instructions | Bound2048 user instructions | Change |
|---|---|---:|---:|---:|
| Build | M1 warm closure,18builds | 96,988,497,773 | 93,322,800,744 | -3.7795% |
| Build | Documents source-core fresh | 2,466,338,499 | 2,466,155,033 | -0.0074% |
| M1 | bfs idiomatic | 85,263,744 | 85,263,609 | -0.0002% |
| M1 | bfs matched | 105,767,050 | 105,767,051 | +0.0000% |
| M1 | generics idiomatic | 64,684,915 | 64,684,957 | +0.0001% |
| M1 | generics matched | 62,531,197 | 62,531,162 | -0.0001% |
| M1 | lru idiomatic | 42,064,609 | 42,064,609 | +0.0000% |
| M1 | lru matched | 51,054,314 | 51,054,336 | +0.0000% |
| M1 | files idiomatic | 1,816,502 | 1,816,557 | +0.0030% |
| M1 | files matched | 1,816,573 | 1,816,728 | +0.0085% |
| M1 | json idiomatic | 98,401,762 | 98,401,731 | -0.0000% |
| M1 | json matched | 75,018,813 | 74,961,986 | -0.0758% |
| M1 | pipeline idiomatic | 16,905,656 | 16,905,675 | +0.0001% |
| M1 | pipeline matched | 11,896,199 | 11,896,180 | -0.0002% |
| M1 | request-scratch idiomatic | 45,206,785 | 45,206,813 | +0.0001% |
| M1 | request-scratch matched | 48,158,883 | 48,158,901 | +0.0000% |
| M1 | rope idiomatic | 13,302,876 | 13,302,883 | +0.0001% |
| M1 | rope matched | 11,982,900 | 11,982,889 | -0.0001% |
| M1 | versions idiomatic | 5,352,343 | 5,352,398 | +0.0010% |
| M1 | versions matched | 3,585,483 | 3,585,480 | -0.0001% |
| Arena | Binarytrees::Obj | 1,669,171 | 1,669,163 | -0.0005% |
| Arena | Binarytrees::Arena | 1,658,424 | 1,658,468 | +0.0027% |
| Arena | Brainfuck::Array | 1,979,053 | 1,979,212 | +0.0080% |
| Arena | Brainfuck::Recursion | 1,986,687 | 1,986,662 | -0.0013% |
| Arena | Matmul::Single | 5,217,492 | 5,217,538 | +0.0009% |
| Arena | Matmul::T4 | 7,391,784 | 7,391,740 | -0.0006% |
| Arena | Matmul::T8 | 8,287,243 | 8,287,219 | -0.0003% |
| Arena | Matmul::T16 | 9,346,157 | 9,346,059 | -0.0010% |
| Arena | Base64::Encode | 18,119,611 | 18,119,718 | +0.0006% |
| Arena | Base64::Decode | 10,935,029 | 10,935,004 | -0.0002% |
| Arena | Json::Generate | 2,050,785 | 2,050,641 | -0.0070% |
| Arena | Json::ParseDom | 2,302,347 | 2,302,210 | -0.0060% |
| Arena | Json::ParseMapping | 2,369,180 | 2,369,017 | -0.0069% |
| Arena | CSV::Parse | 1,829,780 | 1,829,782 | +0.0001% |
| Arena | Etc::Sieve | 1,676,705 | 1,676,570 | -0.0081% |
| Arena | Etc::TextRaytracer | 10,733,436 | 10,733,480 | +0.0004% |
| Arena | Etc::NeuralNet | 53,860,977 | 53,860,948 | -0.0001% |
| Arena | Etc::CacheSimulation | 7,875,693 | 7,875,680 | -0.0002% |
| Arena | Etc::GameOfLife | 2,137,016 | 2,137,043 | +0.0013% |
| Arena | Etc::Words | 3,804,456 | 3,804,611 | +0.0041% |
| Arena | Etc::LogParser | 4,242,264 | 4,242,098 | -0.0039% |
| Arena | Template::Regex | 1,865,503 | 1,865,341 | -0.0087% |
| Arena | Template::Parse | 1,827,793 | 1,827,777 | -0.0009% |
| Arena | Sort::Quick | 73,717,600 | 73,717,634 | +0.0000% |
| Arena | Sort::Merge | 183,839,737 | 183,839,552 | -0.0001% |
| Arena | Sort::Self | 69,495,491 | 69,495,450 | -0.0001% |
| Arena | Graph::BFS | 2,373,512 | 2,373,484 | -0.0012% |
| Arena | Graph::DFS | 2,401,657 | 2,401,630 | -0.0011% |
| Arena | Graph::AStar | 2,355,860 | 2,355,858 | -0.0001% |
| Arena | Hash::SHA256 | 1,733,516 | 1,733,651 | +0.0078% |
| Arena | Hash::CRC32 | 1,702,157 | 1,702,082 | -0.0044% |
| Arena | Calculator::Ast | 1,841,384 | 1,841,221 | -0.0089% |
| Arena | Calculator::Interpreter | 5,212,319 | 5,212,483 | +0.0031% |
| Arena | Maze::Generator | 9,054,734 | 9,054,544 | -0.0021% |
| Arena | Maze::BFS | 8,628,771 | 8,628,789 | +0.0002% |
| Arena | Maze::AStar | 9,504,693 | 9,504,854 | +0.0017% |
| Arena | CLBG::Fannkuchredux | 1,728,124 | 1,728,119 | -0.0003% |
| Arena | CLBG::Mandelbrot | 4,276,958 | 4,276,933 | -0.0006% |
| Arena | CLBG::Nbody | 3,912,578 | 3,912,615 | +0.0009% |
| Arena | CLBG::Spectralnorm | 4,809,271 | 4,809,299 | +0.0006% |
| Arena | Compress::BWTEncode | 1,982,019 | 1,981,877 | -0.0072% |
| Arena | Compress::BWTDecode | 1,745,974 | 1,745,795 | -0.0103% |
| Arena | Compress::HuffEncode | 1,698,302 | 1,698,350 | +0.0028% |
| Arena | Compress::HuffDecode | 1,696,644 | 1,696,465 | -0.0106% |
| Arena | Compress::ArithEncode | 1,701,138 | 1,701,301 | +0.0096% |
| Arena | Compress::ArithDecode | 1,741,944 | 1,741,809 | -0.0077% |
| Arena | Compress::LZWEncode | 2,579,590 | 2,579,572 | -0.0007% |
| Arena | Compress::LZWDecode | 2,526,534 | 2,526,559 | +0.0010% |
| Arena | Distance::Jaro | 1,990,546 | 1,990,728 | +0.0091% |
| Arena | Distance::NGram | 2,725,336 | 2,725,278 | -0.0021% |

The diagnostic cap saves3,665,697,029instructions,**3.7795% of warm M1 build work**. None of the68runtime rows regresses beyond0.01%; the largest measured increase is0.0096% on ArithEncode. JSON matched improves0.0758%;17of18M1executable.text sections are byte-identical. All68measured checksums/stdout/stderr/file observations are exact. This meets the requested >2% warm-build/no-runtime-row-loss criterion on these inputs. The orchestrator approved this policy; the production implementation is qualified for submission.

Documents changes-0.0074%, within the measurement noise: --source-core owns all14bodies and has no library inline imports to prune. Every measured warm M1 build compiles1unit after prewarming the full closure. One serial sample/configuration, CPU16/jobs1/2GiB, shared lock and load<=32. Rust was not rebuilt or re-measured. These are later diagnostic rows; the M2 table stays frozen.

The paired source base is e912f8d5, after a clean rebase over927 site emission. Control compiler becf6b48f8605f4a16965e9c3a495ab423aaf0b2c8221a38a8f74ea3e2da9165; prototype cf505ad1e98e1a7b70bce3b2001850e7c98a57fd924fda6eefd719b3de9ccde6. Shipping runtime d1a4688af43f3d9b7a53e0006c10f75c7314665e52edcf77d448247272c0966a throughout. The later driver default-off row projection was not in this source base. The absolute93.3228G remains above86.604G on this base; the policy does not justify raising that ceiling. Its same-base saving is3.6657G.

## The bound

At release composition, after every supplied unit has linked and imported definitions regain available_externally linkage, count semantic LLVM instructions before O3 and before private result/input adapters. If the imported public function is an infallible wrapper, include its named private .infallible implementation in the count. Above2048, replace only that imported definition with a declaration. Preserve its original name, function type, calling convention and every return/parameter/function attribute; replace address uses too. Owned application/callable variants are untouched. Standalone published-library object emission is untouched. No noinline or new optimization fact is added.

LLVM23InlineCost.h sets the ordinary O3 aggressive threshold to250 weighted cost units. It is not a raw instruction count: target costs, call-site simplification, constant arguments, bonuses and inline hints change the decision. The2048pre-optimization count is a deliberately loose storage/work bound, not an assertion that LLVM cannot inline a larger body. In particular, the emitter initially emits many homes and branches that later disappear. The full measured runtime table, rather than a claimed conversion from2048to250, supplies the evidence for this cap. It can miss a profitable large body on another caller/input; the declaration still runs the same cached public function correctly.

## Remaining import work

After the bound, json retains 1,429,831,624 marginal pass instructions and lru retains 1,045,070,209. Documents has no imports and its 455,448 difference is noise. The two imported rows alone total 2.474901833 G, 2.652% of the measured warm M1 row, above the requested 2% line. OPTIMIZED-IMPORTS.md gives the one-page proposal, cache key and interaction with caller-owned variants. No optimized-bitcode caching is implemented here.

## Qualification and evidence

The pre-SSA component used INLINE_IMPORT_INSTRUCTION_LIMIT=2048. Its boundary test exercised 2047 and 2049 instructions, both as a direct body and as a wrapper plus its private .infallible implementation. It verifies the undefined-symbol path and links and executes each object against the standalone published library definition. The test passed. The native CoreIR suite passed 56 tests with two explicit ignores; the stack-probe test passed. M1 passed 232 cases in each engine/fact configuration, 928 exact observations. Arena passed all50 checksums in each fact mode, with identical outputs.

The 68 run-time observations and two build rows are checked in as LLVM-IMPORT-POLICY.tsv beside this report. These same-base measurements were taken on e912f8d5; later default-off source-row policy changes are outside the pair. Production qualification uses compiler a5262df0c53356bc81746ea501015b820a06a04aa54cd034353600beced99b91 and the same shipping runtime. Residual-import profiling uses this production compiler, a diagnostic preload and private caches. Every measured residual build compiled one application unit. Evidence: import-runtime-{ibase,itest}/rows.json, c1-{ibase,itest}/rows.json, import-land-frozen.json, residual-import-cost/rows.json and the six stage TSVs. Later rows do not replace M2.

Prior art: pinned LLVM23 Analysis/InlineCost.h sets the O3 aggressive threshold to250 weighted cost units; FunctionImport.h tracks definition/declaration choices. Goose's live-specialization C unit does not solve repeated library optimization. Adapted LLVM's selective-import model with a loose raw bound so initial homes and cleanup CFG can simplify before inlining decisions. Published objects continue to own public definitions.
