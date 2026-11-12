# Generic List, ordered Map, String and callback program

Read a List<String>, map a scoped nonretaining callback that appends !, then count mapped strings of at least six UTF-8 bytes and tally them in first-insertion order. Return independent owned strings from the callback. Duplicate map updates preserve order. Drop the map, mapped list and original list in reverse acquisition order. Callback error and failed admission leave no partially published result.

## Input and size

16,384 words, repeated from eight source positions with seven distinct strings including UTF-8 lambda. Idiomatic tally uses HashMap plus insertion-order vector. Matched tally uses one byte arena, u32 key ranges, a contiguous entry vector and open-addressed buckets. Only the private hash wraps explicitly; counts, offsets and output arithmetic are checked.

The committed normal input is 83,968 bytes. `cases.json` gives every input path and variant; no input is generated inside the measured process.

## Oracles and failure variants

Python list/string operations and insertion-ordered dict derive the output. error has !error at callback 2. Capacity fails callback2.string or tally2.key before that operation commits; prior constructed values release with their owner. Failure in the callback never constructs the tally map. Failure in tally releases map before both lists. Borrowed callback arguments never become stored views.

For every case C, `inputs/C.txt` comes from the construction just described and `EXPECTED/C.stdout`, `C.stderr` and `C.exit` come from the corresponding independent Python function in `core/tools/reference.py`. Stderr is empty; exit is 0, 2, 3 or 4 under `core/README.md`. The generator never invokes Rust or Hirsel. The values were fixed before Rust output comparison. Resource names in stdout identify lexical owners, rather than allocator blocks. Named admission failures are logical source sites, never native allocator call numbers. Cleanup does not perform another portable admission.

## Hand check

a,alpha,a,lambda become a!,alpha!,a!,lambda!. The byte lengths are 2,6,2,3, totaling 13; exactly one is at least six bytes. Ordered counts are a!=2, alpha!=1, lambda!=1. Lambda contributes two UTF-8 bytes before the exclamation mark.

## Spec draft correspondence

See `core/SPEC-COMPARISON.md` for this workload's differences from the spec lane's informative example. Runtime expected values come from this contract and the independent references, never from executing that example.
