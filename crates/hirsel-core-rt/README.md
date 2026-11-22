# hirsel-core-rt

Memory operations for FORM v1, independent of the old runtime. Emitters supply
portable `charge-v1` charges and native layout operands separately. Logical
refusal returns `Capacity`; physical host exhaustion is a FAIL-15 run stop.
The native mapping and malloc paths and unrepresentable physical layouts stop with
status 102 and exactly `run stop: HostMemory\n`. Stops bypass cleanup, causes and
join delivery. Destination identity exhaustion uses `run stop: DestinationIds\n`;
all names share the same process stop path (FAIL-15, PROG-3).

The shipping standalone archives select `standalone`: Rust bookkeeping
Box/Vec/maps/String use a System allocator wrapper that stops on a null
allocation, zeroed allocation or reallocation. Payload backing does not pass through it. The embeddable rlib, unit/native fixtures, hirsel_guard
and mimalloc comparison executables keep their host allocator and are outside
this standalone bookkeeping-OOM claim. Cargo standalone archive builds must
select `--features standalone`; never enable it when another Rust crate owns
the global allocator. Both shipping and optional observed Buck archives select
it explicitly. Failed page-size queries stop as PageSize; missing installed
signal state stops as Signal. Implementation-invariant panics remain bugs.

`src/abi.rs` is the emitter reference. `include/hirsel_core_rt.h` publishes the
same C declarations and frozen 64-bit offsets. Success Capacity fields are zero.
`rt.dest_id` takes no context or status and returns a fresh u64 identity.

List, String and Map carriers own storage exactly when capacity is nonzero.
Capacity-zero data may point to read-only static bytes at any length. Cleanup
skips that buffer; growth admits from current, copies length bytes and never
frees it. Raw allocation/free/refund entries take owned blocks, not carriers.

FORM's consuming `grow` admits the full replacement charge against the old
payer, copies the live elements, installs the carrier, then settles that
payer's old charge. A refusal leaves the carrier intact. Large buffers call a
shared entry. Small buffers inline the same allocation and copy sequence.

Each task has a paged limit table and a direct current-record pointer. Admission,
refund and scope operations use ordinary task-local loads and stores. Outstanding
bytes retain payer slots; only scope handles and union edges need structural
pins. A pointer free list owns each reusable record pin. Scope entry transfers
that pin, reserves the bound once and sets current. A no-survivor exit returns
unused room and the record without union bookkeeping. Survivor handover and
page growth use cold paths; ordinary scope entry allocates no heap record.

Each task owns size-class queues and recycles empty pages. A page groups one
payer and native class, including alignment and cleanup storage. Plain slots
contain only the payload. Fixed nonleaf blocks use spare words within or after
the payload for resumable cleanup. Dynamic buffers keep a 40-byte frame, and
resources reserve a state pointer. The page records the payer and native kind.
A transferred block keeps its page and allocating heap alive until its last
free. A task-local lock serializes queue operations and foreign returns.
Dedicated descriptors live outside the mapping and use a sharded address
index. Purging a dedicated mapping discards its payload and ownership prefix.
Ordinary slabs claim aligned slots from bitmap-managed ranges shared across
heaps. A returned slot gives back its resident pages, and the last slot
releases the range.

Regions bump within nonmoving pools keyed by native size and alignment. The
first chunk rounds the request to a granule. Each pool grows geometrically
within the region-wide backing ceiling. Closing refunds the aggregate payer
runs and returns chunks to the task for reuse, retiring at most one chunk beyond
the last two nonempty uses. A compact builder
remembers its region entry and retained charge. Trim takes those explicit
operands and needs no allocation header. Writer/copy/index code and
Child/Bounds/Overflow checks belong to the emitter.

Rc/Arc share and nonlast release inline; last release uses allocation-free,
constant-stack resumable glue. Recipes decrement retained children inline and
continue locally; only a dead descendant returns to the intrusive worklist.
The traversal keeps depth-first refund order. A worklist node funded by the
current canonical record refunds directly before deallocation; other payers
retain the existing forwarded or remote settlement path. A scheduler binds native task execution on resume
and unbinds before moving it to another worker. Foreign refunds accumulate in
prepaid per-slot mailboxes until the payer task's event. Resuming only binds
execution custody and caches the running task's inbox. Growth admits from the
current limit (MEM-26). A received grant belongs to the finalizing task until
hand-over. The old owner folds pending credits before hand-over, and the
recipient applies the returned reservation and joins the records at its next
event.
Guard bounds cache the immutable page extent when each stack is mapped, so a
normal scheduler switch performs no sysconf page-size query.

The optional `mimalloc` backend supplies region-chunk backing. Own blocks use
the same task allocator in every shipping archive.

Buck target `//:hirsel-core-rt-archives` builds x86-64 and AArch64 archives with
normal Cargo dependencies and no old-runtime feature matrix. Rust fixtures run
through Buck. Reproduce costs with `python3 crates/hirsel-core-rt/measure.py
--out <new-directory>`; `--abi-only` runs the C header/static-link fixture.

`include/hirsel_core_rt_host.h` declares process entry and the host provider ABI.
The driver pins a Config with immutable argv, explicit grants, the provider and
source-bound diagnostic callbacks. The generated entry thunk calls main,
cleans its returned error and live owners, and then writes MainResult. Only
hirsel_core_run maps that completed outcome to PROG-3's process status.
Before live or recording entry, an OS Process grant installs SIGTERM and
SIGINT handlers. The first signal latches the stop and wakes the provider
through the scheduler's readiness path. A second signal takes the default
crash action. The build's recording switch selects the runtime archive.
The recording archive includes the `record` feature.
A replay tool supplies the stop from the recording and leaves the signal
dispositions alone.

Provider callbacks receive a Request at issue and an Outcome at delivery. A
replay adapter supplies the result at issue without calling the OS adapter.
Read and mkdtemp append into the caller's Buffer capacity and update its len;
write reports committed bytes even on error. No operation admits language
storage. IoError tags and operation numbers are fixed in the process header.
The OS adapter pins configured directory grants and uses Linux openat2 with
RESOLVE_BENEATH for every path traversal. Before any host effect, open, mkdir,
remove and remove_tree reject NUL, absolute paths and empty/dot/dotdot components
with Denied. Mkdtemp takes a basename prefix, permits empty/dot/dotdot prefixes,
rejects slash or NUL with Denied, and creates the prefix plus an alphanumeric
suffix. Only the suffix is appended, within the 32 prepaid spare bytes; collision
retries stay within one issue and stop after 128 attempts. Unsupported kernels
report ENOSYS.
It permits links that stay beneath a grant and never follows links during
remove_tree. Host descriptor tables belong to the run, outside task limits.

`include/hirsel_core_rt_tasks.h` declares the generic task/channel ABI. Emitters
specialize the frozen FORM rows with ValueLayout constants and drop thunks;
task bodies accept context, result destination and the transferred input slot.
Task argument/result storage and channel rings are prepaid. The scheduler
executes stackful tasks on one worker, switches only at blocking operations or
explicit checkpoints, and records completed events through SchedulerProvider.
All root and child stacks have a guard and page-probed language frames. The
OS adapter runs non-suspending libc calls on a separate guarded host stack.
The common register switch lives in hirsel-stack.

The cross-task memory hooks live in abi_tasks.rs. Task end forwards surviving funding
to the parent. Cross-task refunds publish one event and enter prepaid pending slots, which
task_resume drains before an admission. Foreign growth drains under the exclusive
payer lease and publishes its successful charge. Child Context lives inside Task;
zero-sized input/result slots allocate no heap storage. Empty child reservations
return their slot directly instead of building a union edge. Event callbacks cannot suspend or re-enter; provider
issue/delivery callbacks can suspend on their task's own stack.

FORM decision 44 fixes every capability and Args operand as a plain u64 provider
handle. OsConfig.parameters lists main's parameter kinds and ordinals in source
order; the C constructor fills Config.grants[i] with those handles and refuses
missing grants before startup. Core adapters resolve provider metadata through
the task context. Args accessors take context and its u64 Args handle. Run closes
all grants after main cleanup and diagnostics, without admission or failure;
os_process_free then releases host argv/metadata. No program drop visits a grant.

Inline count offsets remain count=0, payload=8, aligned to 8 on both targets.
Rc initialization/counts use ordinary u64 stores. Arc initialization uses an
atomic u64 initialized to 1 before publication; share uses checked relaxed CAS,
release uses release decrement plus an acquire fence at zero, and unwrap loads
with acquire. The frozen context event word at offset8 is nonzero in a running task,
including without a recorder: failed Arc unwrap must reach poll scheduling. Emitters test it after each Arc operation and call the context-first
hirsel_core_arc_initialized/shared/released/unwrapped on the nonzero
branch, before last-release glue. The word points at this pinned task when enabled.
The root sets it before execution and each child inherits recording but installs
its own task pointer before publication. It is immutable until the task's run ends.
The hook reads task/run directly from the word, without TLS. A failed unwrap
can schedule after the poll allowance; hooks return no status and admit nothing.
The scheduler holds its event turn across the count operation and hook.
Acceptance builds enable memcheck to register alternate stacks; Rust tests do so
automatically. Normal archives carry no Valgrind stack-registration work.

Own free obtains the original payer from the block's page. The runtime refunds
the current record directly on pointer equality, or follows forwarding and
the existing remote refund path. It then returns the slot to its allocating
task.

FORM #50 provider rows use `hirsel_core_host_op(context, op, handle, operand,
mode, site)`, with HostOp codes listed in the host header. Operand points to a String/Bytes carrier;
only read/mkdtemp update its length. Capacity zero borrows storage regardless of
the pointer or length. No provider call frees, grows or admits carrier storage.
Open returns a provider-aware HostFile; mkdtemp returns a provider-aware Dir.
Outcome fields are value at 0, IoError tag at 8, code at 12, written at 16 and
failure status at 24. Waiting rows expose status; clocks use only value.
`hirsel_core_args_get(context, args, index)` returns a pointer to a pinned
capacity-zero carrier or null for None. The process constructor copies argv and
owns these carriers for the run; Config.args is now their immutable table.

Cause readers borrow the native cell's core layout directly: kind at 0, the
inline String carrier at 8 and site at 32, total 40 bytes. The next-cell pointer
follows at 40; native funding is outside this prefix. No translation or message
drop callback exists. Dropping a chain, including an unjoined failed child,
frees moved String backing exactly when capacity is nonzero. Prepaid logical
cell charges and append order remain unchanged. abi_layout.rs and the C header
publish these offsets; primitive raise passes the complete 40-byte value.

CONC-10 poll fairness uses one allowance of 64 polls per task. The only polls are
try_send Full, no-request check_cancel, Arc unwrap returning its holder, and
Capacity refusal. No-request checks and Capacity have event codes 16 and 17.
Successful operations never consume the allowance.
Poll results are fixed before scheduling and never observe cancellation. Runnable
pollers defer until already-ready peers have a turn; the provider records its
choice among eligible peers. This is not a loop-backedge yield or a time quantum.
MEM-20 foreign refunds stay in prepaid mailboxes until the owning task's event.
Resuming only binds/normalizes custody. Draining a shared domain at an event
applies only the current task's pending slots; other owners retain theirs.

Empty event paths inspect the Task's direct atomic refund_pending flag, with
no Context/payer/domain traversal. The mailbox's registered notification flags
are set only by foreign refund publication or payer forwarding. A set flag
enters cold drain_refunds; clearing it and unregistering share the mailbox
mutex with notification, so neither a wake nor a flag lifetime can be lost.
The subscription ends before the task Context can release its domain. Binding
caches an owner inbox without applying refunds; successful admission stays unchanged.
The no-request cancellation branch
is inline; observing an actual request, draining and changing turns are cold.

Populated refunds use one intrusive dirty list per union-find root, with links
prepaid in payer slots, and each task owner's inbox lists the roots with dirty
slots. Multiple refunds to one slot accumulate in its single dirty entry;
owner events drain only their inbox, without scanning clean records or another
owner's refunds. The bound-inbox pending flag also prevents foreign-growth
allocation paths from repeatedly scanning someone else's queue. Local worker
publication notifies only the target owner. A foreign OS thread cannot read
union records: it queues an unassigned entry and notifies owners; the next owner
event routes those entries under the table lease. The domain mutex serializes
publication, flag reset, forwarding and subscription removal. Handover routes
queued entries to their new owner without delivering them.

Refund routing v2 (refund-reroute-1465; reversible by restoring the v1
per-owner list and its whole-inbox walk): every queued slot that is not
unassigned is on the dirty list of its current root, and a root's list is
nonempty exactly when the root is listed on one inbox, that of its live
owner. A union gives the winner the loser's list and puts it on the winner's
live inbox. A hand-over joins the child's list to the destination root's.
Reassigning a root's owner (`task_owner`) moves only that root's list. Each
of these costs O(1) and visits no other root's slots, so a teardown that
reassigns N grants costs O(N), not O(N²). Delivery is unchanged: only the
owning task pops its inbox, and an inbox that lists no root and has no
unassigned entry loses its notification. Live records and the
bound cache pin inbox metadata; zero-pin inboxes enter an intrusive reap list
and are removed under the next local mailbox lock. Metadata therefore follows
live records and task registrations rather than historical task IDs.

Ordinary emitted allocation checks the overflow operand and admits positive
charges against the current record. For a nonnull Like, it reads the payer
from the owning page first. Real overflow, shortage, zero charge and foreign
Like use `hirsel_core_alloc` once. Test injection probes a fitting admission
without double-counting a cold-path attempt.

Reserved append qualification uses `core/tools/memory_append.py --builder
<release-hot_loops> --runtime <shipping-archive> --out <fresh-directory> --check`.
It measures List, Bytes and String loops with reserved capacity, checks every
result and asserts that no grow helper executes. A cold grow edge remains in the
compiled loop. The LLVM instruction envelopes reject the carrier-escape regression
introduced by 4316fbb2; Cranelift counts are reported alongside it. Run serially
with the other counter samples and the same CPU16 / 2 GiB envelope. This is an
explicit performance qualification, not a wall-time test in the unit suite.
The separate `grow-carrier-words.hirsel` oracle forces moving realloc and checks
success, refusal and regrowth, including both data and capacity copy-back.
