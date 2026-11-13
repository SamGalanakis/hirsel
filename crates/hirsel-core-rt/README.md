# hirsel-core-rt

Memory operations for FORM v1, independent of the old runtime. Emitters supply
portable `charge-v1` charges and native layout operands separately. Logical
refusal returns `Capacity`; physical host exhaustion is a FAIL-15 run stop.

`src/abi.rs` is the emitter reference. `include/hirsel_core_rt.h` publishes the
same C declarations and frozen 64-bit offsets. Success Capacity fields are zero.
`rt.dest_id` takes no context or status and returns a fresh u64 identity.

Each task has a paged limit table and a direct current-record pointer. Admission,
refund and scope operations use ordinary task-local loads and stores. Outstanding
bytes retain payer slots; only scope handles and union edges need structural
pins. A pointer free list owns each reusable record pin. Scope entry transfers
that pin, reserves the bound once and sets current. A no-survivor exit returns
unused room and the record without union bookkeeping. Survivor handover and
page growth use cold paths; ordinary scope entry allocates no heap record.

Plain blocks have one native payer word. Graph nodes additionally reserve an
intrusive resumable destruction frame; resources reserve a state pointer. The
allocator takes a native kind; kind and alignment tag the payer word so generic
free needs only pointer and charge. These tags never affect logical charges. No legacy ABI is retained.

Regions bump within nonmoving chunks and aggregate charges per payer run, with
four inline prior entries. A compact builder remembers its region entry and
retained charge. Trim takes those explicit operands and never needs an object
header. Writer/copy/index code and Child/Bounds/Overflow checks belong to the
emitter. Region close refunds the aggregate entries and frees chunks.

Rc/Arc share and nonlast release inline; last release uses allocation-free,
constant-stack resumable glue. A scheduler binds native task execution on resume
and unbinds before moving it to another worker. Foreign refunds accumulate in
prepaid per-slot mailboxes and are drained before resuming the payer task. MEM-26
also makes growth of a received collection an ordered foreign charge. The task
provider's payer_lease hook drains earlier refunds before foreign growth; received
grants execute as the finalizing task until handover. One OS worker supplies the
exclusive lease. With multiple workers, the scheduler must quiesce every task
sharing the payer table for growth and for the entire received finalizer, including
suspension. An empty mailbox bypasses locking and scanning at task_resume.

System allocation is the default. The optional `mimalloc` backend uses native
malloc/free for ordinary alignment and aligned allocation above 16 bytes. The
measurement driver compares both backends, retaining Rust's upstream MiMalloc
wrapper as its Rust baseline. `MEASUREMENTS.md` records costs and limitations.

Buck target `//:hirsel-core-rt-archives` builds x86-64 and AArch64 archives with
normal Cargo dependencies and no old-runtime feature matrix. Rust fixtures run
through Buck. Reproduce costs with `python3 crates/hirsel-core-rt/measure.py
--out <new-directory>`; `--abi-only` runs the C header/static-link fixture.

`include/hirsel_core_rt_host.h` declares process entry and the host provider ABI.
The driver pins a Config with immutable argv, explicit grants, the provider and
source-bound diagnostic callbacks. The generated entry thunk calls main,
cleans its returned error and live owners, and then writes MainResult. Only
hirsel_core_run maps that completed outcome to PROG-3's process status.

Provider callbacks receive a Request at issue and an Outcome at delivery. A
replay adapter supplies the result at issue without calling the OS adapter.
Read and mkdtemp append into the caller's Buffer capacity and update its len;
write reports committed bytes even on error. No operation admits language
storage. IoError tags and operation numbers are fixed in the process header.
The OS adapter pins configured directory grants and uses Linux openat2 with
RESOLVE_BENEATH for every path traversal. Unsupported kernels report ENOSYS.
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
The common register switch lives in hirsel-stack and also serves hirsel-rt.

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
with acquire. Emitters call hirsel_core_arc_initialized/shared/released/unwrapped after each Arc operation, before
last-release glue. The hook returns no status and admits/allocates nothing.
The scheduler holds its event turn across the count operation and hook.
Acceptance builds enable memcheck to register alternate stacks; Rust tests do so
automatically. Normal archives carry no Valgrind stack-registration work.

Own fast paths can retain the current payer as a compiler fact. If no operation
can change that record between allocation and free, the emitter refunds it
directly and skips prefix lookup/comparison, as inline_own_free_current shows.
Otherwise inline_own_free compares the original payer and follows its forwarding
record through the slow refund when needed. Both preserve MEM-20/MEM-26.
Canonical admission and the plain/align8 prefix store need no tag mask.
