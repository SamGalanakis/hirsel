# Sort prior art

## Stable sort

`list.sort_stable` (`core/core/list.hirsel`) adapts Rust core's stable driftsort at
commit `a7c37f9f0ee5e93396106e9de20f4a86b4154e63`. References are under
`library/core/src/slice/sort/`: `stable/mod.rs:29-93` (entry), `stable/drift.rs:20-260`
(Powersort over lazy logical runs, `merge_tree_depth`, `sqrt_approx`, `logical_merge`,
`create_run`), `stable/quicksort.rs:16-240` (bounded stable quicksort, ancestor
equality, `stable_partition`, `PartitionState`), `stable/merge.rs:8-140` (shorter-run
merge, `merge_up`, `merge_down` and the drop guard), `shared/pivot.rs:13-100`
(`choose_pivot`, `median3_rec`, `median3`), `shared/mod.rs:20-50`
(`find_existing_run`) and `shared/smallsort.rs:174-191,536-606,754` (thresholds,
`insert_tail`, `sort4_stable`, `bidirectional_merge`).

Hirsel elements are owned and may be move-only, and the comparator may fail at any
call. One scratch allocation of n slots (`slots_new`, its byte charge admitted
before any call) holds elements in flight. A `Hole` guard records the single
operation that holds scratch slots at a comparator call (merge up, merge down or
insertion, partition, small-sort merge) and its drop returns exactly those slots to the holes in the
list, so every element stays owned once on every exit. Core loops take no Steps;
each callback entry is one Step, so failure sites are comparator calls only.

Per mechanism:

- Powersort merge order with the scaled leading-zero merge-tree depth: ADOPTED.
  The run stack packs length, depth and the sorted bit into one `Inline[u64, 66]`.
- `min_good_run_len` (half the input up to 64, then `sqrt_approx` above 4096): ADOPTED.
- Lazy unsorted logical runs, joined while unsorted and quicksorted when they meet a
  sorted run: ADOPTED. Eager mode for inputs of at most twice the small-sort threshold
  and for the quicksort fallback: ADOPTED, sorting runs of the threshold (40 here, 32
  in Rust) with the small sort below.
- Run detection (strictly descending reversed, nondescending kept): ADOPTED.
- Stable quicksort with a 2 floor(log2 n) bad-partition budget and eager drift
  fallback: ADOPTED. Rust recurses on the right side; here the larger side waits on
  an explicit pending stack and the loop takes the smaller, because the evaluator gives
  each spawned task 64 KiB of stack: ADAPTED.
- Ancestor-pivot equal partition: ADAPTED. Rust copies the ancestor pivot (Freeze) or
  falls back to `left_partition_len == 0`. Here the right range keeps the ancestor's
  index, which is stable until that range is first partitioned; left ranges use the
  `left_partition_len == 0` rule. No element is copied.
- Branchless stable partition (left keys fill scratch upward, right keys downward,
  right block reversed on copy-back; the pivot is never compared with itself):
  ADOPTED, with checked indices.
- Four-way unroll of the partition scan (`stable_partition`, `UNROLL_LEN = 4`):
  ADAPTED. Rust unrolls only when `size_of::<T>() <= 16`; Hirsel cannot ask the size
  of `T`, so every element type takes the unrolled loop and pays its code size. Each
  step still updates the Hole state before the next comparison.
- `choose_pivot` / `median3_rec` / `median3`: ADOPTED, as indices into the list.
- Shorter-run buffered merge with branchless source selection (`merge_up`,
  `merge_down`): ADOPTED. Rust's `MergeState` drop becomes the `Hole` restore arms.
- Merge trimming by galloping (CPython `Objects/listsort.txt`, "Galloping"): ADAPTED.
  Rust merges whole runs. Before a merge, an exponential then binary search from the
  run boundary finds the left keys not after the right run's first key and the right
  keys not before the left run's last; only the overlap is merged. On the nearly
  sorted rounds of LangArena BWT this cut instructions by 29%. The searches compare
  in place with no hole open. TimSort's in-merge galloping mode: NOT TAKEN.
- Insertion for whole lists of at most 20 (`MAX_LEN_ALWAYS_INSERTION_SORT`): ADOPTED,
  as a hole insertion expressed as a one-element merge down so it shares that restore
  arm.
- `small_sort_general` for quicksort leaves and eager runs: ADAPTED. The threshold is
  40, not Rust's 32: on 510c749d, 24, 28, 32, 40, 48 and 64 moved LangArena Sort::Self
  instructions by +2.49%, +1.09%, +0.15%, -0.75%, -0.71% and -0.32%, and 40 also cut
  BWTEncode 1.30% and BWTDecode 0.91%.
  Each half starts from a `sort4_stable` network, grows by `insert_tail` and the halves
  meet in one `bidirectional_merge`. The network makes all five comparisons before it
  moves a key, so a failure inside it holds nothing. Rust builds the halves in scratch
  with bitwise copies; here they are built in xs, so only the merge holds scratch (two
  runs at once, restored by their own `Hole` arm). Rust's merge reads past an exhausted
  run (Freeze copies) and panics on an order violation at the end; here the rounds stop
  when either run empties and the other run's remainder fills the gap, so every
  comparator yields a permutation with no check. `sort8_stable`: NOT TAKEN (measured
  +3.1% Sort::Self instructions at threshold 32, cycles unchanged).
- Uninitialized `MaybeUninit` scratch, 4 KiB stack buffer, 8 MB full-scratch cap and
  type-size specialization: REPLACED by one n-slot `Slots[T]` allocation, which
  keeps the single allocation and byte charge of the previous sort.

The failure laws in `core/core/tests/list_sort_failure.hirsel` fail the comparator
at every call of lists that reach each phase (natural runs, small sorts, partitions,
equal partitions, merges), audit the permutation after each failure, and expect the
complete sort's call count from an independent replay model of this algorithm.
Per-call twins on the evaluator check scratch and list slot liveness, and each
restore arm has laws that go red when it is removed.

## Unstable partition storage

The existing pattern-defeating quicksort and its comparator sequence remain in
`stdlib/std@1/list.hir`. Its 64-position block queues use the bitmap mechanism
from LLVM libc++ commit `fc9215428138c4d9238486c2bdbea192b6107b65`,
`libcxx/include/__algorithm/sort.h:354–399` (`__swap_bitmap_pos`,
`__populate_left_bitset`, `__populate_right_bitset`) and `452–480`
(`__swap_bitmap_pos_within`). A set bit represents one misplaced element.
Trailing-zero counts consume paired positions in ascending order; leading-zero
counts consume remaining positions in descending order. This replaces two
initialized `Inline[u64, 64]` arrays with two `u64` values.

Rust std was also reviewed at commit
`6a58677913ae3e443146503de3933abef0b2eda3`,
`library/core/src/slice/sort/unstable/quicksort.rs:90–152` (partition contract
and dispatch), `244–337` (cyclic branchless Lomuto), and `341–360` (simple
Lomuto). Its cyclic moves and different comparison sequence are not ported:
Hirsel must keep every owner in the list and preserve observable callback
failure, including the first failing site.

Deviations from libc++:

- Keep the existing Hirsel scans, block sizes, pivot selection, recursion,
  heapsort fallback and exact comparison/swap order. Only queue storage changes;
  libc++'s whole partitioner and comparator-purity dispatch are not adopted.
- Compare through the public three-way noalloc callback, with ordinary failure
  propagation. Classify all selected left positions before the right positions,
  then swap. No callback is skipped or assumed infallible.
- Use checked source indexing and ownership-preserving `list.swap`; no unchecked
  pointer operations, temporary element copies, backend assumptions or validator
  exceptions are introduced. Each shift is by a block offset below 64, and each
  subtract-one mask update is guarded by a nonzero test.
- Clear the highest remaining bit with XOR rather than constructing a mask below
  that bit. This keeps the highest-bit case within ordinary checked u64 shifts.

Existing Zig literal vectors, six input patterns and stable-sort fixtures remain
independent controls. Added record and failure fixtures pin a 129-element
pre-change equal-key permutation and the first of two distinct callback failure
sites. The permutation pin is a regression control for this storage change,
not a new public stability guarantee.

### Partition pivot loans

Block classification now borrows `xs[begin]` once per nonempty left or right
block, following libc++'s borrowed `__pivot` parameter in the same pinned
`sort.h:372–399`. The loan's scope ends before the first ownership-preserving
swap. All element index arithmetic, comparisons, bitmap updates and swaps keep
their original order. Empty blocks do not read the pivot.

This needs no callback-purity promise. `xs` enters through exclusive unique
storage; its elements are borrowed read-only by the comparator (OWN-1/OWN-3).
No swap or other list mutation occurs within either classification block.
The pivot index is the nonempty range's `begin`, and every scan index lies
strictly above it and below `end <= len`. Hoisting that checked pivot read
therefore cannot expose a bounds failure before a previously observable
callback failure. Ordinary comparator failures still propagate at their
original comparator source sites. Generic elements are borrowed, never copied
or moved into a temporary owner. Stable sort and the public signatures are
unchanged.

LLVM can remove the repeated pivot bounds check and address calculation, and
vectorize a concrete infallible integer comparison where its existing rules
permit it. Fallible or generic callbacks retain their ordinary call sequence;
no compiler, IR, runtime or callback contract changes are involved.
