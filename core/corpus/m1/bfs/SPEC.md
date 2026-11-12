# Breadth-first graph search returning an owned path

Read vertex count, source, destination and a directed edge list in encounter order. Mark a vertex when enqueued; visit neighbors in input order. Exhaust the reachable component, then return a newly owned source-to-target path. There is no borrowed result into scratch. Validate vertices before search allocation. Return NoPath for an unreachable target.

## Input and size

16,384 vertices and 65,536 directed edges. Each vertex links to offsets 1,7,64,511 modulo V. The query is 0 to 16,383. Idiomatic Rust uses Vec<Vec<usize>>; matched Rust builds u32 compressed adjacency offsets and neighbors. Both have separate parent and queue buffers of V elements.

The committed normal input is 697,566 bytes. `cases.json` gives every input path and variant; no input is generated inside the measured process.

## Oracles and failure variants

Python traverses layer frontiers and stores complete paths rather than parent links. small is a six-vertex tie-order witness. error is an unreachable destination; invalid has an out-of-range edge. Capacity variants reject search.parent, search.queue and result.path before allocation. Already acquired queue then parent then graph release in that order. No path is printed if output admission fails.

For every case C, `inputs/C.txt` comes from the construction just described and `EXPECTED/C.stdout`, `C.stderr` and `C.exit` come from the corresponding independent Python function in `core/tools/reference.py`. Stderr is empty; exit is 0, 2, 3 or 4 under `core/README.md`. The generator never invokes Rust or Hirsel. The values were fixed before Rust output comparison. Resource names in stdout identify lexical owners, rather than allocator blocks. Named admission failures are logical source sites, never native allocator call numbers. Cleanup does not perform another portable admission.

## Hand check

The frontier from 0 is 1,2, then 3,4, then 5. Vertex 3 first comes from 1; 5 first comes from 3. Thus the returned path is 0,1,3,5, and all six vertices are visited. The equally short alternative through 2,4 loses by declared encounter order.

## Spec draft correspondence

See `core/SPEC-COMPARISON.md` for this workload's differences from the spec lane's informative example. Runtime expected values come from this contract and the independent references, never from executing that example.
