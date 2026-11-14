| Allocation IR slice | Control instructions | Candidate instructions | Change |
|---|---:|---:|---:|
| Documents fresh | 2,312,414,218 | 2,311,266,692 | -0.0496% |
| M1 warm closure,18builds | 87,186,605,375 | 87,044,120,995 | -0.1634% |

Only production IR construction changes: omit the test-only admission probe/refusal blocks and their Phi inputs, branch straight to current-payer admission, and avoid an explicit constant-false overflow conjunction. The test engine retains injected refusal paths and exact admission counts. Every actual size, overflow, payer, Capacity and region-live-payer check remains; no new fact or runtime call shape.

The paired source base is5a8990b4 with compiler a5262df0c53356bc81746ea501015b820a06a04aa54cd034353600beced99b91 before and73e53eb9037f3bbba527566bec533701bb758f5672adad5810083d7157219c05 after, shipping runtime d1a4688a. Later source changes are outside the pair. One serial sample/configuration, CPU16/jobs1/2GiB and load<=32, private caches, complete library closure warm; all18measured builds compile1unit each. Every M1 .text hash differs, so executable-byte parity is not claimed. The68runtime counter/output pair is unchanged within noise, total1,305,866,676->1,305,864,798instructions, worst+.0106% files idiomatic, best-.0110% BinarytreesObj. No runtime tradeoff is established. M2 stays frozen and no hard ceiling is raised.

Current rebased native CoreIR suite59pass/3explicitARMignores; M1232each engine/factmode,928exact observations; Arena50each factmode exact, reusing the measured on observations and running off once. Admission/refusal and front agreement bodies are green. The initial native invocations were not started because generated Buck inputs were stale; the one completed suite is the qualification. Logs are allocation-native-qualified.log,allocation-qualification.log,allocation-arena.log; counters in c1-allocation-{base,on} and import-runtime-allocation-{base,on}.

This third scaffold slice saves only1.148M documents instructions; the first empty-edge and status pairs saved9.004M and6.716M. Those first three contributions are below the orchestrator's2%series checkpoint, across explicitly different source bases. They do not meet the5%series goal. The raw-bitcode product has priority; allocator free/refund factoring and SSA homes remain later candidates rather than an invented cumulative total.
