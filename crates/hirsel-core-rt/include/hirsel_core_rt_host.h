#ifndef HIRSEL_CORE_PROCESS_H
#define HIRSEL_CORE_PROCESS_H
#include "hirsel_core_rt.h"
/* Provider operations: Open 0, Read 1, Write 2, Close 3, TryLock 4, Unlock 5,
   Mkdir 6, Mkdtemp 7, Remove 8, RemoveTree 9, WallMs 10, MonoNs 11,
   Stdout 12, Stderr 13, Rename 14, SleepUntil 15, Sync 16, SyncDir 17, Random 18.
   Open modes: Read 0, Write 1, Append 2, CreateNew 3. */
typedef struct { uint8_t tag; uint32_t code; } HirselCoreIoError;
/* IoError: 0 success, 1 NotFound, 2 Denied, 3 Exists, 4 Closed, 5 Other(code). */
typedef struct { uint8_t *data; size_t len, capacity; } HirselCoreBuffer;
typedef struct {
    uint8_t operation, mode;
    uint64_t handle;
    const uint8_t *path;
    size_t path_len;
    HirselCoreBuffer buffer;
    uint64_t site;
} HirselCoreHostRequest;
typedef struct {
    uint64_t value;
    HirselCoreIoError error;
    uint64_t written;
    uint8_t status;
} HirselCoreHostOutcome;
/* Four-state provider ABI (C cancelled at entry, W waiting, I issued, D delivered).
   Wait kind: 0 Ready, 1 Until(deadline_ns), 2 Park. Step kind: 0 Done, 1 Pending
   (deadline_ns 0 is none). Deadlines are host CLOCK_MONOTONIC nanoseconds. abort why:
   1 Cancel, 2 Deadline. All five callbacks are required: hirsel_core_run refuses a
   provider with a NULL one ("run stop: Provider", status 102) before main. wait,
   resume and abort are called by the scheduler at the moment of a wake, outside any
   task, and must not block; issue and deliver run in the task; deliver runs exactly
   once per operation that reaches D. */
typedef struct { uint8_t kind; uint64_t deadline_ns; } HirselCoreWait;
typedef struct { uint8_t kind; HirselCoreHostOutcome outcome; uint64_t deadline_ns; } HirselCoreStep;
typedef struct {
    void *state;
    HirselCoreWait (*wait)(void *, HirselCoreHostRequest *, uint64_t);
    HirselCoreStep (*issue)(void *, HirselCoreHostRequest *, uint64_t);
    HirselCoreStep (*resume)(void *, HirselCoreHostRequest *, uint64_t);
    HirselCoreHostOutcome (*abort)(void *, HirselCoreHostRequest *, uint64_t, uint8_t);
    HirselCoreHostOutcome (*deliver)(void *, HirselCoreHostRequest *, HirselCoreHostOutcome);
} HirselCoreProvider;
/* Readies the Park or Pending operation `token` (unique per operation in the
   process). Any thread, task or callback may call it; the operation's scheduler
   re-runs wait or resume at its next turn, before any task is popped. Readies that
   arrive together are one wake; an unknown or finished token is ignored; a ready
   that arrives before the operation parks applies at its next park. */
void hirsel_core_ready(uint64_t token);
HirselCoreHostOutcome hirsel_core_host(HirselCoreContext *, const HirselCoreProvider *, HirselCoreHostRequest *);
/* FORM #50: outcome layout value@0, error.tag@8, error.code@12, written@16,
   status@24 (failure convention 3c242a8a). error 0 succeeds; 1..5 are IoError
   case + 1. Clock rows ignore status and use value; waiting rows can fail with
   Cancelled, mkdtemp also Bounds. Only read/mkdtemp update operand.len; data
   and capacity never change. Capacity 0 borrows storage (even if len > 0).
   No row admits or grows/frees carrier storage. Null operand for no operand. */
HirselCoreHostOutcome hirsel_core_host_op(HirselCoreContext *, uint8_t op, uint64_t handle, HirselCoreBuffer *operand, uint8_t mode, uint64_t site);
HirselCoreHostOutcome hirsel_core_host_rename(HirselCoreContext *, uint64_t handle, HirselCoreBuffer *from, HirselCoreBuffer *to, uint64_t site);
HirselCoreHostOutcome hirsel_core_host_sleep_until(HirselCoreContext *, uint64_t handle, uint64_t deadline_ns, uint64_t site);
/* HostFile (FORM #47): u64 provider handle; no cell, grant or charge.
   No public fields or inline offsets. OPEN returns it in
   Outcome.value; file operations use its retained issuing provider. CLOSE
   consumes it even on error/cancellation. A key cannot escape its run. */
typedef uint64_t HirselCoreHostFile;
HirselCoreHostOutcome hirsel_core_host_file_open(HirselCoreContext *, const HirselCoreProvider *, HirselCoreHostRequest *);
HirselCoreHostOutcome hirsel_core_host_file(HirselCoreContext *, HirselCoreHostFile, HirselCoreHostRequest *);
/* Drop consumes a live key once: no admission, cancellation or failure result;
   ignores close errors (MEM-24), never retries close. Remaining files close
   before grants at normal/failure exit; run stops skip cleanup (_exit closes
   descriptors). OS calls retain the guarded foreign-call stack rule. */
void hirsel_core_hostfile_drop(HirselCoreContext *, HirselCoreHostFile);
typedef struct { const uint8_t *data; size_t len; } HirselCoreView;
typedef struct { uint8_t kind, exit; } HirselCoreMainResult;
typedef struct { uint8_t kind; uint64_t task, object, value; } HirselCoreEvent;
typedef struct {
    void *state;
    void (*event)(void *, HirselCoreEvent);
    size_t (*choose)(void *, size_t runnable_count);
} HirselCoreSchedulerProvider;
typedef struct { uint8_t kind; const HirselCoreProvider *provider; uint64_t handle; } HirselCoreEntryGrant;
typedef struct { uint8_t kind; size_t index; } HirselCoreEntryRequirement;
/* Grant kinds: Dir 0, Net 1, Clock 2, Random 3, Stdout 4, Stderr 5,
   Stdin 6, Env 7, Process 8, Tasks 9, Args 10. Capability/Args operands
   are plain u64 provider handles, eight physical/logical bytes in FORM. */
typedef struct HirselCoreProcessConfig {
    uint64_t root_limit;
    const HirselCoreBuffer *args;
    size_t args_len;
    const uint64_t *grants;
    size_t grants_len;
    const HirselCoreProvider *provider;
    void *state;
    void (*diagnostic)(void *, const uint8_t *, size_t);
    HirselCoreView (*describe)(void *, const HirselCoreCause *);
    HirselCoreSchedulerProvider scheduler;
    int32_t stop_fd; /* zero selects inherited stderr */
    const HirselCoreEntryGrant *capabilities;
    size_t capabilities_len;
    void (*close_grants)(void *); /* after main cleanup; admits nothing, cannot fail */
} HirselCoreProcessConfig;
typedef uint8_t (*HirselCoreEntry)(HirselCoreContext *, const HirselCoreProcessConfig *, HirselCoreMainResult *);
uint8_t hirsel_core_run(const HirselCoreProcessConfig *, HirselCoreEntry);
uint64_t hirsel_core_args_len(const HirselCoreContext *, uint64_t args);
/* Null means None; otherwise one pinned capacity-0 String carrier per argument,
   borrowed until the run ends. The caller never frees it or its bytes. */
const HirselCoreBuffer *hirsel_core_args_get(const HirselCoreContext *, uint64_t args, uint64_t index);
uint8_t hirsel_core_check_cancel(HirselCoreContext *, uint64_t site);
/* Constructor fills cfg.grants[i] in main parameter order from requirements.
   Main takes one uint64_t per parameter. entry_grant returns zero when missing.
   Args uses the same u64 operand convention; args_len/get also take context. */
uint64_t hirsel_core_entry_grant(const HirselCoreProcessConfig *, uint8_t kind, size_t index);
bool hirsel_core_entry_grants_present(const HirselCoreProcessConfig *, const HirselCoreEntryRequirement *, size_t);
const HirselCoreProvider *hirsel_core_capability_provider(const HirselCoreContext *, uint64_t);
uint64_t hirsel_core_capability_handle(const HirselCoreContext *, uint64_t);
typedef struct {
    uint64_t root_limit;
    const HirselCoreView *args;
    size_t args_len;
    const HirselCoreView *directories;
    size_t directories_len;
    uint64_t grant_mask; /* bit k grants kind k, 1..9; Dirs use explicit paths */
    int32_t stdout_fd, stderr_fd;
    const HirselCoreEntryRequirement *parameters;
    size_t parameters_len;
} HirselCoreOsConfig;
typedef struct HirselCoreOsProcess HirselCoreOsProcess;
typedef struct { HirselCoreOsProcess *process; HirselCoreIoError error; } HirselCoreOpenProcess;
/* Copies argv, pins directories, duplicates stream descriptors. A failed
   constructor closes every acquired descriptor. No process owner on error. */
HirselCoreOpenProcess hirsel_core_os_process_new(const HirselCoreOsConfig *);
/* Named OS entry. parameter_names has one borrowed UTF-8 view per requirement.
   First Dir requires one successful confined openat2 probe before main; no Dir
   never probes. Setup refusal: status 103 and exactly one "not started: " stderr
   line naming the parameter. The raw constructor reports IoError without text.
   No syscall probe or extra work is added to an open operation. */
#define HIRSEL_CORE_NOT_STARTED 103
uint8_t hirsel_core_os_run(const HirselCoreOsConfig *, const HirselCoreView *parameter_names, HirselCoreEntry);
const HirselCoreProcessConfig *hirsel_core_os_process_config(const HirselCoreOsProcess *);
/* Run closes grants after main cleanup, without admission/failure. After run
   and every borrowed view ends, free releases argv/metadata and unused setup. */
void hirsel_core_os_process_free(HirselCoreOsProcess *);
#include "hirsel_core_rt_sites.h"
#endif
