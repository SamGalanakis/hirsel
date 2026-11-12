# Correspondence with the spec lane's draft

Compared directly with the draft at
`/home/sam/.t3/worktrees/hirsellang/t3-837f2de3-22280e0c230eb9f2/docs/spec/examples.md`
on 2026-11-12, before measuring. These are gaps or deliberate workload specializations;
none is silently treated as an executed v1 program. The spec lane owns Hirsel sources.

All nine examples omit input fixtures, complete entry points, exact stdout/exit oracles,
portable allocation-failure sites and cleanup traces. They cannot yet run this corpus
without those additions. They contain no full normal/error/Capacity matrix; only example
5 describes cancellation. This corpus supplies that matrix independently. No Hirsel
source or expected value was copied from the old conformance corpus.

1. Request scratch: both use immutable Arc configuration and request-local stable data.
   The draft parses a Socket and a Head containing path/length; this fixture uses a
   scripted request provider, multiple headers and a content header to exercise scratch.
   It does not measure real network throughput. The draft catches Capacity at the bound
   boundary and writes `HTTP/1.1 413\r\n\r\n`; this corpus returns Capacity and releases
   scratch then socket, with no 413 delivery. It also stops the finite plan at its first
   request failure. Its normal response is banner:id:content, whereas the draft leaves
   `render` unspecified. The draft's parse signature only returns Capacity, so it does
   not describe malformed-header or missing-route errors. The 64 KiB bound is an example
   plan; this corpus fixes logical injection sites without inventing canonical charges.

2. JSON: both use immutable document storage and anchored path lookup. The draft's
   printer handles Num and Arr, emits no array punctuation, and prints an ellipsis for
   all other cases. The workload implements the full JSON grammar, duplicate-key
   last-wins policy, escaped strings, objects, punctuation and two-space pretty printing.
   The draft only shows one-level object `field`; the corpus queries multiple field and
   array-index steps. The draft does not state duplicate policy, finite-number errors,
   complete ParseError/Capacity effects or input sizes. Corpus numbers preserve valid
   fixture lexemes rather than testing a general f64 shortest-format contract. Matched
   Rust uses an active-tag compact blob with u32 offsets, as the draft intends.

3. LRU: both have an indexed generation-checked entry pool and recency links. The draft
   shows only get/touch and returns payload length. This corpus adds prepare/install,
   variable-size payloads, eviction, stale saved handles and complete drain. It returns
   copied hit bytes to the fixture driver so their checksum observes payload contents.
   Both have O(1) linked-list recency operations; the draft does not give a capacity or
   slot-reuse policy. This corpus fixes 64 entries and bounded key/value dimensions.

4. BFS: the draft stores List<List<u32>> and leaves traversal and walk_back as helpers.
   This corpus fixes directed edge input order, full reachable-component traversal,
   enqueue-time marking, tie-breaking, InvalidVertex, NoPath and caller-owned output
   admission. Idiomatic Rust matches the draft adjacency lists; matched Rust uses CSR,
   as PROGRAMS.md requires. The draft gives no defined tie order or NoPath observation;
   its Option result can represent that case but this fixture reports a typed NoPath.

5. Pipeline: the draft and corpus use Q=8, three tasks, ownership transfer and uppercase
   batches. Source/sink/main, allocation failure and cancellation scheduling are absent
   from the draft's code. The corpus provides a finite plan and test-only rendezvous
   gates, proves Full on q1, and specifies cancellation at a blocked send, the next
   receive, and an explicit transform check. The receive cancellation fixture combines
   a pending scripted cancellation with source-end closure to wake Rust's receive;
   Rust std channels have no native cancellation operation. Its failure sink starts
   after transform cleanup so release order is exact. Normal task scheduling order is
   not part of stdout. It does not add pure-code polling. The draft's one-MiB task grants
   are not a chosen Rust allocator limit; only logical admission sites are compared.

6. Versions: the draft has `Arc[Version { n, data: Map }]`; it describes root pinning but
   no persistent path-copy update algorithm or prompt shared-node reclamation witness.
   This workload uses an immutable balanced tree, explicit path copies and live-node
   counts before/after unpin. Values are u64 and keys are u32 for a fixed topology,
   rather than String keys. The whole map is never cloned on update. The draft's
   publication error returns the untransferred Version; the corpus stages a draft and
   rejects its third node allocation, keeping both published roots unchanged. There
   is one writer and one retained reader pin; concurrent publication is not measured.

7. Text: both use counted immutable pieces and an owning undo list. The draft only
   shows snapshot; the corpus supplies UTF-8 byte-range replacement, piece splitting,
   bounded history eviction, undo, invalid positions and live-chunk release. It uses a
   piece table with descriptor-list snapshots, matching the shown List<Rc<Piece>>,
   rather than claiming a balanced rope's O(log n) edit cost. Payload slices share a
   chunk, so replacing text never mutates the old snapshot or performs implicit COW.

8. Files: both use two files and an actual OS file lock; mutex poisoning is excluded.
   The draft's normal path explicitly consumes destination before unlock, giving
   destination, lock, source release order. PROGRAMS.md gives lock, destination, source
   for normal automatic scope exit. This corpus adopts the latter on success and error.
   It opens the source and creates destination, as the draft does, rather than opening
   two existing files. It adds acquisition failures, a visible partial-copy prefix,
   named buffer Capacity, cleanup-report failures and primary/suppressed failure order.
   Cleanup injection reports failure after actual OS release, so it does not claim that
   a failing external release itself succeeded. No durability or rollback is promised.

9. Generic APIs: the draft independently shows count_long and tally. This corpus also
   instantiates a fallible generic map callback that constructs owned String results,
   uses min=6 UTF-8 bytes, and tallies mapped strings in first-insertion order. It fixes
   callback and map-key Capacity sites plus partial-prefix cleanup. The draft does not
   specify order for its Map output or exercise list.map construction; neither omission
   is used to infer a different map order or escaping callback permission.

The common new rulings are preserved: no semantic depth counter, no typed CountLimit,
no implicit sharing/COW and no shared user mutex. Saturated counts and exhausted native
stack are environmental run stops and do not become recoverable fixture errors.
