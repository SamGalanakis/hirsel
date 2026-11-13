# hirsel-core-rt

Memory operations for FORM v1, independent of the old runtime. Logical charges
are supplied by the emitter using `hirsel-core-layout::charge`. Allocation
refusal returns the plain C `Capacity` record. Physical host exhaustion is a
run stop, as specified by FAIL-15.

The C declarations are in `include/hirsel_core_rt.h`. Pointers and recipes come
from our emitters. They must obey the ownership, layout and threading contracts
documented on the Rust entry points. No runtime validation of compiler output
is performed. Task/provider code must serialize cross-task funding operations
in its recorded event order; the funding forest itself is synchronized.

Bookkeeping precedes each own payload. Counted payloads start with the FORM
u64 count at offset zero and user data at offset eight. Native padding and
bookkeeping never contribute to logical charges.

`//:hirsel-core-rt-archives` builds `libhirsel_core_rt.a` for x86-64 and AArch64,
using this crate's normal Cargo dependencies. Each target also has an `x86_64`
or `aarch64` subtarget. This namespace has no old-runtime feature variants or
old-language startup training. The emitters supply resumable mechanical drop
glue through the ABI modules.

`MEASUREMENTS.md` records the instruction samples and missed targets. Reproduce
them with `python3 crates/hirsel-core-rt/measure.py --out <new-directory>` from
the repo root. `--abi-only` instead compiles and runs `tests/abi.c` against the
published header and a static archive. Ordinary Rust tests run through Buck.

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
to the parent. Cross-task refunds update the effective payer, then publish one
event. The current synchronized funding forest settles directly; a task-local
representation drains its prepaid pending slots in task_resume before code can
attempt another admission. Event callbacks cannot suspend or re-enter; provider
issue/delivery callbacks can suspend on their task's own stack.

FORM decision 44 fixes every capability and Args operand as a plain u64 provider
handle. OsConfig.parameters lists main's parameter kinds and ordinals in source
order; the C constructor fills Config.grants[i] with those handles and refuses
missing grants before startup. Core adapters resolve provider metadata through
the task context. Args accessors take context and its u64 Args handle. Run closes
all grants after main cleanup and diagnostics, without admission or failure;
os_process_free then releases host argv/metadata. No program drop visits a grant.
