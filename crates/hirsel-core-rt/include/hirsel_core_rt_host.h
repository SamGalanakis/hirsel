#ifndef HIRSEL_CORE_PROCESS_H
#define HIRSEL_CORE_PROCESS_H
#include "hirsel_core_rt.h"
/* Provider operations: Open 0, Read 1, Write 2, Close 3, TryLock 4, Unlock 5,
   Mkdir 6, Mkdtemp 7, Remove 8, RemoveTree 9, WallMs 10, MonoNs 11,
   Stdout 12, Stderr 13, Rename 14, SleepUntil 15, Sync 16, SyncDir 17, Random 18,
   Connect 19, Listen 20, Accept 21, SendPair 26, RecvBounded 27, Stopped 28.
   ReadAt 37, WriteAt 38, Resolve 40, OpenDir 43, Entries 44, NextEntries 45,
   Stat 46, StatFile 47, CloseEntries 48, SleepUntilWall 77, RecvReady 78, Mkstemp 119, ZeroExtension 124.
   Entries takes an optional path (path_len 0 means the grant root).
   Positioned requests carry an eight-byte little-endian
   offset in path and the original open mode in mode.
   Open modes: Read 0, Write 1, Append 2, CreateNew 3, ReadWrite 4. */
typedef struct { uint8_t tag; uint32_t code; } HirselCoreIoError;
/* IoError: 0 success, 1 NotFound, 2 Denied, 3 Exists, 4 Closed, 5 Other(code).
   Network outcomes use 1 Denied, 2 Refused, 3 Unreachable, 4 InUse, 5 Reset,
   6 TimedOut, 7 Aborted, 8 Other(code). */
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
    uint8_t status, kind;
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
   status@24 (failure convention 3c242a8a), kind@25. Stat returns size in value,
   signed modification nanoseconds in written, and the file kind in kind. error 0 succeeds; 1..5 are IoError
   case + 1. Clock rows ignore status and use value; waiting rows can fail with
   Cancelled. Mkdtemp, Mkstemp, positioned I/O and NextEntries can also fail with Bounds.
   Read, ReadAt, Mkdtemp, Mkstemp and NextEntries can update operand.len; data and
   capacity never change. Capacity 0 borrows storage (even if len > 0).
   No row admits or grows/frees carrier storage. Null operand for no operand. */
HirselCoreHostOutcome hirsel_core_host_op(HirselCoreContext *, uint8_t op, uint64_t handle, HirselCoreBuffer *operand, uint8_t mode, uint64_t site);
HirselCoreHostOutcome hirsel_core_host_io(HirselCoreContext *, uint8_t op, uint64_t handle, HirselCoreBuffer *operand, uint8_t mode, uint64_t site);
HirselCoreHostOutcome hirsel_core_host_clock(HirselCoreContext *, uint8_t op, uint64_t handle, HirselCoreBuffer *operand, uint8_t mode, uint64_t site);
HirselCoreHostOutcome hirsel_core_host_stat(HirselCoreContext *, uint8_t op, uint64_t handle, HirselCoreBuffer *operand, uint8_t mode, uint64_t site);
HirselCoreHostOutcome hirsel_core_host_at(HirselCoreContext *, uint8_t op, uint64_t handle, uint64_t at, HirselCoreBuffer *operand, uint64_t site);
HirselCoreHostOutcome hirsel_core_host_zero_at(HirselCoreContext *, uint64_t file, uint64_t at, uint64_t len, uint64_t site);
HirselCoreHostOutcome hirsel_core_host_rename(HirselCoreContext *, uint64_t handle, HirselCoreBuffer *from, HirselCoreBuffer *to, uint64_t site);
HirselCoreHostOutcome hirsel_core_host_sleep_until(HirselCoreContext *, uint64_t handle, uint64_t deadline_ns, uint64_t site);
HirselCoreHostOutcome hirsel_core_host_sleep_until_wall(HirselCoreContext *, uint64_t handle, uint64_t wall_ms, uint64_t site);
/* HostFile (FORM #47): u64 provider handle; no cell, grant or charge.
   No public fields or inline offsets. OPEN returns it in
   Outcome.value; file operations use its retained issuing provider. CLOSE
   consumes it even on error/cancellation. A key cannot escape its run. */
typedef uint64_t HirselCoreHostFile;
HirselCoreHostOutcome hirsel_core_host_file_open(HirselCoreContext *, const HirselCoreProvider *, HirselCoreHostRequest *);
HirselCoreHostOutcome hirsel_core_host_file(HirselCoreContext *, HirselCoreHostFile, HirselCoreHostRequest *);
/* Drop consumes a live file or listing key once, with no admission or cancellation;
   ignores close errors (MEM-24), never retries close. Remaining files close
   before grants at normal/failure exit; run stops skip cleanup (_exit closes
   descriptors). OS calls retain the guarded foreign-call stack rule. */
void hirsel_core_hostfile_drop(HirselCoreContext *, HirselCoreHostFile);
typedef struct { const uint8_t *data; size_t len; } HirselCoreView;
/* text/text_len: PROG-3's text of a non-Exit error, set by hirsel_core_main_error
   before the result is cleaned; the run prints and frees it. */
typedef struct { uint8_t kind, exit; uint8_t *text; size_t text_len; } HirselCoreMainResult;
typedef struct { uint8_t kind; uint64_t task, object, value; } HirselCoreEvent;
/* CONC-10 event kinds, one number each: hirsel_core_layout::event, which a runtime
   test compares with this table. */
enum {
    HIRSEL_CORE_EVENT_SPAWN=0, HIRSEL_CORE_EVENT_END=1, HIRSEL_CORE_EVENT_CANCEL=2,
    HIRSEL_CORE_EVENT_CANCELLED=3, HIRSEL_CORE_EVENT_JOIN=4, HIRSEL_CORE_EVENT_PUBLISH=5,
    HIRSEL_CORE_EVENT_RECEIVE=6, HIRSEL_CORE_EVENT_TRY_SEND=7, HIRSEL_CORE_EVENT_SENDERS=8,
    HIRSEL_CORE_EVENT_CLOSE=9, HIRSEL_CORE_EVENT_ARC_NEW=10, HIRSEL_CORE_EVENT_ARC_SHARE=11,
    HIRSEL_CORE_EVENT_ARC_RELEASE=12, HIRSEL_CORE_EVENT_ARC_UNWRAP=13,
    HIRSEL_CORE_EVENT_REFUND=14,
    HIRSEL_CORE_EVENT_CHECK_CANCEL=16, HIRSEL_CORE_EVENT_CAPACITY=17,
    HIRSEL_CORE_EVENT_LATEST_PUBLISH=18, HIRSEL_CORE_EVENT_LATEST_READ=19,
    HIRSEL_CORE_EVENT_TRY_RECEIVE=20, HIRSEL_CORE_EVENT_STEP_SLICE=21
};
typedef struct {
    void *state;
    void (*event)(void *, HirselCoreEvent);
    size_t reserved;
} HirselCoreSchedulerProvider;
typedef struct { uint8_t kind; const HirselCoreProvider *provider; uint64_t handle; bool terminal; } HirselCoreEntryGrant;
typedef struct { uint8_t kind; size_t index; } HirselCoreEntryRequirement;
/* Grant kinds: Dir 0, Net 1, Clock 2, Random 3, Stdout 4, Stderr 5,
   Stdin 6, Env 7, Process 8, Tasks 9, Args 10. Capability/Args operands
   are plain u64 provider handles, eight physical/logical bytes in FORM.
   For Net, EntryGrant.handle designates a pinned HirselCoreNetRules list;
   OsConfig copies its explicit rules into that immutable run-owned grant. */
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
    const void *launch; /* immutable hirsel_core_ids::launch::Metadata, pinned for the run */
    bool (*describe_output)(void *, const uint8_t *, size_t);
} HirselCoreProcessConfig;
typedef uint8_t (*HirselCoreEntry)(HirselCoreContext *, const HirselCoreProcessConfig *, HirselCoreMainResult *);
uint8_t hirsel_core_run(const HirselCoreProcessConfig *, HirselCoreEntry);
void hirsel_core_main_error(HirselCoreContext *, const uint8_t *value, const uint8_t *program,
                            size_t len, HirselCoreMainResult *);
uint64_t hirsel_core_args_len(const HirselCoreContext *, uint64_t args);
uint64_t hirsel_core_launch_word(const HirselCoreContext *, uint64_t field, uint64_t index);
void hirsel_core_launch_target(const HirselCoreContext *, HirselCoreBuffer *output);
bool hirsel_core_describe_emit(const HirselCoreContext *, const HirselCoreBuffer *);
/* Null means None; otherwise one pinned capacity-0 String carrier per argument,
   borrowed until the run ends. The caller never frees it or its bytes. */
const HirselCoreBuffer *hirsel_core_args_get(const HirselCoreContext *, uint64_t args, uint64_t index);
uint8_t hirsel_core_check_cancel(HirselCoreContext *, uint64_t site);
/* Constructor fills cfg.grants[i] in main parameter order from requirements.
   Main takes one uint64_t per parameter. entry_grant returns zero when missing.
   Args uses the same u64 operand convention; args_len/get also take context. */
HirselCoreHostOutcome hirsel_core_stream_is_terminal(HirselCoreContext *, uint64_t, uint8_t, uint64_t);
HirselCoreHostOutcome hirsel_core_stream_size(HirselCoreContext *, uint64_t, uint8_t, uint64_t);
uint64_t hirsel_core_entry_grant(const HirselCoreProcessConfig *, uint8_t kind, size_t index);
bool hirsel_core_entry_grants_present(const HirselCoreProcessConfig *, const HirselCoreEntryRequirement *, size_t);
const HirselCoreProvider *hirsel_core_capability_provider(const HirselCoreContext *, uint64_t);
uint64_t hirsel_core_capability_handle(const HirselCoreContext *, uint64_t);
/* NET-6 configuration. dir: 0 Connect, 1 Listen, 2 Tls, 3 Resolve. Address
   rules use family (0 V4, 1 V6), hi, lo and bits. Tls and Resolve use
   comma-separated patterns. Tls also uses trust (0 System, 1 Roots) and
   roots for the SHA-256 pin. Resolve uses only patterns and patterns_len.
   NET-12 extends dir with 4 Unix and 5 UnixRoot (launch input only). For
   those rows, patterns contains the concatenated label and path, and hi
   gives the label's byte count. A Unix row's family is its direction
   (0 Connect, 1 Listen, 2 Bind, 3 Send). NET-15 extends address directions
   with 6 UdpBind and 7 UdpSend. */
typedef struct {
    uint8_t dir, family;
    uint64_t hi, lo;
    uint8_t bits;
    uint8_t reach; /* NET-18: 0 Any, 1 Public (Connect only) */
    uint16_t first, last;
    const uint8_t *patterns;
    size_t patterns_len;
    uint8_t trust;
    uint8_t roots[32];
} HirselCoreNetRule;
typedef struct { const HirselCoreNetRule *data; size_t len; } HirselCoreNetRules;
typedef struct HirselCoreDirectoryOperations HirselCoreDirectoryOperations;
extern const HirselCoreDirectoryOperations hirsel_core_os_directory_ops;
typedef struct HirselCoreProcessOperations HirselCoreProcessOperations;
extern const HirselCoreProcessOperations hirsel_core_os_process_ops;
typedef struct HirselCoreSocketActivation HirselCoreSocketActivation;
extern const HirselCoreSocketActivation hirsel_core_os_socket_activation;
typedef struct HirselCoreSecretOperations HirselCoreSecretOperations;
extern const HirselCoreSecretOperations hirsel_core_os_secret_ops;
typedef struct HirselCoreEnvOperations HirselCoreEnvOperations;
extern const HirselCoreEnvOperations hirsel_core_os_env_ops;
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
    const HirselCoreNetRule *net_rules;
    size_t net_rules_len;
    HirselCoreView target;
    /* Set to &hirsel_core_os_directory_ops for a sourced launch with directories. */
    const HirselCoreDirectoryOperations *directory_ops;
    /* Set to &hirsel_core_os_process_ops for a sourced launch with Process. */
    const HirselCoreProcessOperations *process_ops;
    /* Set to &hirsel_core_os_socket_activation for a sourced launch with Net. */
    const HirselCoreSocketActivation *socket_activation;
    const HirselCoreView (*secret_grants)[3]; /* parameter, source label, patterns */
    size_t secret_grants_len;
    /* Set to &hirsel_core_os_secret_ops for a sourced launch with Secrets. */
    const HirselCoreSecretOperations *secret_ops;
    HirselCoreView env_patterns;
    /* Set to &hirsel_core_os_env_ops for a sourced launch with Env. */
    const HirselCoreEnvOperations *env_ops;
    const void *exec_bindings;
    size_t exec_bindings_len;
    const void *exec_ops;
    const HirselCoreView (*cred_grants)[3];
    size_t cred_grants_len;
    const uint8_t (*revision)[32];
    /* Nonzero when the program reaches the runner package (place-only launch words). */
    uint64_t place;
} HirselCoreOsConfig;
typedef struct HirselCoreOsProcess HirselCoreOsProcess;
typedef struct { HirselCoreOsProcess *process; HirselCoreIoError error; } HirselCoreOpenProcess;
/* Copies argv, pins directories, duplicates stream descriptors. A failed
   constructor closes every acquired descriptor. No process owner on error.
   Matching socket activation transfers fd 3 and consumes LISTEN_* metadata
   at startup, before other threads can access the environment. */
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

/* Resolve appends Ip elements (tag@0, payload@8, stride 24). The result
   counts appended elements; the request buffer counts elements rather than bytes. */
HirselCoreHostOutcome hirsel_core_host_resolve(HirselCoreContext *, uint64_t, const HirselCoreBuffer *, HirselCoreBuffer *, uint8_t, uint64_t, uint64_t, uint64_t);
HirselCoreHostOutcome hirsel_core_net_narrow_resolve(HirselCoreContext *, uint64_t, const HirselCoreBuffer *, uint64_t);
#define HIRSEL_CORE_CRED_FETCH 80
HirselCoreHostOutcome hirsel_core_cred_fetch(HirselCoreContext *, uint64_t, HirselCoreBuffer *, HirselCoreBuffer *, uint8_t, uint64_t, uint64_t, uint64_t);
/* Until is lowered to kind (0 Never, 1 At), Clock capability, absolute ns.
   Listen/accept append normalized address scalars. Split has no replay point. */
typedef struct {
    uint64_t value;
    HirselCoreIoError error;
    uint64_t written;
    uint8_t status, family;
    uint64_t hi, lo;
    uint16_t port;
} HirselCoreAddressOutcome;
HirselCoreHostOutcome hirsel_core_host_connect(HirselCoreContext *, uint64_t, uint8_t, uint64_t, uint64_t, uint16_t, uint8_t, uint64_t, uint64_t, uint64_t);
HirselCoreAddressOutcome hirsel_core_net_local_addr(HirselCoreContext *, uint64_t, uint64_t);
HirselCoreAddressOutcome hirsel_core_net_peer_addr(HirselCoreContext *, uint64_t, uint64_t);
HirselCoreAddressOutcome hirsel_core_host_listen(HirselCoreContext *, uint64_t, uint8_t, uint64_t, uint64_t, uint16_t, uint16_t, uint64_t);
HirselCoreAddressOutcome hirsel_core_host_accept(HirselCoreContext *, uint64_t, uint8_t, uint64_t, uint64_t, uint64_t);
/* Unix-socket and datagram adapters: context, handle, operands, site. */
HirselCoreHostOutcome hirsel_core_host_connect_unix(HirselCoreContext *, uint64_t, const HirselCoreBuffer *root, const HirselCoreBuffer *path, uint8_t, uint64_t, uint64_t, uint64_t);
HirselCoreHostOutcome hirsel_core_host_listen_unix(HirselCoreContext *, uint64_t, const HirselCoreBuffer *root, const HirselCoreBuffer *path, uint16_t backlog, uint64_t);
HirselCoreHostOutcome hirsel_core_host_accept_unix(HirselCoreContext *, uint64_t, uint8_t, uint64_t, uint64_t, uint64_t);
HirselCoreHostOutcome hirsel_core_host_open_datagram(HirselCoreContext *, uint64_t, uint64_t);
HirselCoreHostOutcome hirsel_core_host_bind_datagram(HirselCoreContext *, uint64_t, const HirselCoreBuffer *root, const HirselCoreBuffer *path, uint64_t);
HirselCoreHostOutcome hirsel_core_host_connect_datagram(HirselCoreContext *, uint64_t, const HirselCoreBuffer *root, const HirselCoreBuffer *path, uint64_t);
HirselCoreHostOutcome hirsel_core_host_send_datagram(HirselCoreContext *, uint64_t, const HirselCoreBuffer *bytes, uint8_t, uint64_t, uint64_t, uint64_t);
HirselCoreHostOutcome hirsel_core_host_send_to(HirselCoreContext *, uint64_t, const HirselCoreBuffer *root, const HirselCoreBuffer *path, const HirselCoreBuffer *bytes, uint8_t, uint64_t, uint64_t, uint64_t);
HirselCoreHostOutcome hirsel_core_host_recv_datagram(HirselCoreContext *, uint64_t, HirselCoreBuffer *bytes, uint8_t, uint64_t, uint64_t, uint64_t);
HirselCoreHostOutcome hirsel_core_net_narrow_unix(HirselCoreContext *, uint64_t builder, uint8_t direction, const HirselCoreBuffer *root, const HirselCoreBuffer *path, uint64_t);
/* UDP: bind (family, hi, lo, port, slots, slot_bytes[, members, steer, out]). */
HirselCoreHostOutcome hirsel_core_host_udp_open(HirselCoreContext *, uint64_t net, bool v6, uint32_t slots, uint32_t slot_bytes, uint64_t);
HirselCoreAddressOutcome hirsel_core_host_udp_bind(HirselCoreContext *, uint64_t net, uint8_t, uint64_t, uint64_t, uint16_t, uint32_t, uint32_t, uint64_t);
HirselCoreAddressOutcome hirsel_core_host_udp_bind_group(HirselCoreContext *, uint64_t net, uint8_t, uint64_t, uint64_t, uint16_t, uint32_t, uint32_t, uint16_t members, uint32_t steer, HirselCoreBuffer *out, uint64_t);
HirselCoreAddressOutcome hirsel_core_host_udp_local_addr(HirselCoreContext *, uint64_t key, uint64_t);
HirselCoreHostOutcome hirsel_core_host_udp_recv(HirselCoreContext *, uint64_t key, HirselCoreBuffer *bytes, HirselCoreBuffer *packets, uint8_t, uint64_t, uint64_t, uint64_t);
HirselCoreHostOutcome hirsel_core_host_udp_send(HirselCoreContext *, uint64_t key, const HirselCoreBuffer *bytes, const HirselCoreBuffer *packets, uint8_t, uint64_t, uint64_t, uint64_t);
HirselCoreHostOutcome hirsel_core_udp_fill_packets(HirselCoreContext *, uint64_t served_key, uint64_t call_key, const HirselCoreBuffer *packets, uint64_t);
HirselCoreHostOutcome hirsel_core_exec_narrow(HirselCoreContext *, uint64_t exec, uint32_t, const HirselCoreBuffer *name, const HirselCoreBuffer *argument, uint64_t);
/* FORM TLS connect: the outcome and four network-order peer digest words. */
typedef struct { HirselCoreHostOutcome outcome; uint64_t peer[4]; } HirselCoreTlsConnectOutcome;
HirselCoreTlsConnectOutcome hirsel_core_tls_connect(HirselCoreContext *, uint64_t, uint64_t key, const HirselCoreBuffer *name, uint8_t trust, const HirselCoreBuffer *roots, const HirselCoreBuffer *alpn, uint8_t, uint64_t, uint64_t, uint64_t);
/* RECV_BOUNDED (27): max_receive clips the request buffer's writable spare
   capacity. The caller's carrier publishes only the delivered length. */
HirselCoreHostOutcome hirsel_core_host_recv_bounded(HirselCoreContext *, uint64_t, HirselCoreBuffer *, uint64_t max_receive, uint8_t, uint64_t, uint64_t, uint64_t);
/* RECV_READY (78): no buffer. Completes with status 0 once the read half's
   next receive has bytes, end of stream or an error to report, and consumes
   none of them; TimedOut (6) at the deadline. A provider may complete it
   early (the following receive then waits). Issue and delivery pass the
   replay point as RECV_BOUNDED's do. */
HirselCoreHostOutcome hirsel_core_host_recv_ready(HirselCoreContext *, uint64_t, uint8_t, uint64_t, uint64_t, uint64_t);
/* STDIN_READ (32): append at most the caller's existing spare capacity. */
HirselCoreHostOutcome hirsel_core_stdin_read(HirselCoreContext *, uint64_t, HirselCoreBuffer *, uint8_t, uint64_t, uint64_t, uint64_t);
/* STDIN_SECRET (182): one echo-off terminal line, sealed by the host under
   the policy in the String carrier. Outcome.value is the Secret's
   designation. Errors: 1 NotTerminal, 2 Busy, 3 Empty, 4 TooLong, 5 Eof,
   6 TimedOut, 7 Aborted, 8 Other(errno), 9 the policy (code 1 Invalid, code 2
   Capacity with value requested and written available). */
HirselCoreHostOutcome hirsel_core_stdin_read_secret(HirselCoreContext *, uint64_t, const HirselCoreBuffer *, uint8_t, uint64_t, uint64_t, uint64_t);
/* Env get and name return a borrowed carrier address in Outcome.value (zero for None). */
HirselCoreHostOutcome hirsel_core_env_get(HirselCoreContext *, uint64_t, const HirselCoreBuffer *, uint64_t);
HirselCoreHostOutcome hirsel_core_env_len(HirselCoreContext *, uint64_t, uint64_t);
HirselCoreHostOutcome hirsel_core_env_name(HirselCoreContext *, uint64_t, uint64_t, uint64_t);
HirselCoreHostOutcome hirsel_core_env_narrow(HirselCoreContext *, uint64_t, const HirselCoreBuffer *, uint64_t);
typedef struct {
    HirselCoreHostOutcome outcome;
    uint64_t stdin_pipe, stdout_pipe, stderr_pipe;
} HirselCoreSpawnOutcome;
HirselCoreSpawnOutcome hirsel_core_exec_spawn(HirselCoreContext *, uint64_t, const HirselCoreBuffer *, const HirselCoreBuffer *, const HirselCoreBuffer *, const HirselCoreBuffer *, const HirselCoreBuffer *, uint8_t, uint8_t, uint8_t, uint64_t);
HirselCoreHostOutcome hirsel_core_exec_wait(HirselCoreContext *, uint64_t, uint8_t, uint64_t, uint64_t, uint64_t);
HirselCoreHostOutcome hirsel_core_exec_signal(HirselCoreContext *, uint64_t, uint8_t, uint64_t);
/* provide cells (core.md, Provide). Constructors admit one cell from the
   current limit; a refusal returns value 0 with written = available. Clock
   answers (value = Clock grant, written = ClockHand key), random answers its
   Random grant, process answers (value = Process grant, written = StopTrigger
   key). The last argument of each call is the site. */
HirselCoreHostOutcome hirsel_core_provide_clock(HirselCoreContext *, uint64_t mono, uint64_t wall, uint64_t);
HirselCoreHostOutcome hirsel_core_provide_random(HirselCoreContext *, uint64_t seed, uint64_t);
HirselCoreHostOutcome hirsel_core_provide_random_tape(HirselCoreContext *, const HirselCoreBuffer *, uint64_t);
HirselCoreHostOutcome hirsel_core_provide_process(HirselCoreContext *, uint64_t);
HirselCoreHostOutcome hirsel_core_clock_advance_to(HirselCoreContext *, uint64_t hand, uint64_t mono, uint64_t);
HirselCoreHostOutcome hirsel_core_clock_set_wall(HirselCoreContext *, uint64_t hand, uint64_t wall, uint64_t);
/* Suspends until every other task waits, then Outcome.value is the earliest
   waited deadline and written is 1 when there is one. */
HirselCoreHostOutcome hirsel_core_clock_idle(HirselCoreContext *, uint64_t hand, uint64_t);
HirselCoreHostOutcome hirsel_core_process_trigger(HirselCoreContext *, uint64_t trigger, uint64_t);
/* served ports (core.md, Provide). The constructors answer value = the
 * capability's grant and written = the Served key; next answers written 1 with
 * value = the Call key, or written 0 at quiescence; reply answers value 1 for
 * Withdrawn. */
HirselCoreHostOutcome hirsel_core_provide_stdout(HirselCoreContext *, uint64_t n, uint64_t size, uint64_t);
HirselCoreHostOutcome hirsel_core_provide_stderr(HirselCoreContext *, uint64_t n, uint64_t size, uint64_t);
HirselCoreHostOutcome hirsel_core_port_next(HirselCoreContext *, uint64_t served, uint64_t);
HirselCoreHostOutcome hirsel_core_call_word(HirselCoreContext *, uint64_t call, uint64_t i, uint64_t);
HirselCoreHostOutcome hirsel_core_call_secret(HirselCoreContext *, uint64_t call, uint64_t index, uint64_t);
HirselCoreHostOutcome hirsel_core_call_payload(HirselCoreContext *, uint64_t call, uint64_t field, HirselCoreBuffer *out, uint64_t);
HirselCoreHostOutcome hirsel_core_call_payload_prefix(HirselCoreContext *, uint64_t call, uint64_t field, uint64_t max, HirselCoreBuffer *out, uint64_t);
HirselCoreHostOutcome hirsel_core_port_reply(HirselCoreContext *, uint64_t served, uint64_t call, uint64_t tag, uint64_t w0, uint64_t w1, uint64_t w2, uint64_t w3, uint64_t w4, uint64_t w5, uint64_t);
HirselCoreHostOutcome hirsel_core_port_fill(HirselCoreContext *, uint64_t served, uint64_t call, const HirselCoreBuffer *bytes, uint64_t);
HirselCoreHostOutcome hirsel_core_provide_dir(HirselCoreContext *, uint64_t n, uint64_t);
HirselCoreHostOutcome hirsel_core_provide_net(HirselCoreContext *, uint64_t n, uint64_t);
HirselCoreHostOutcome hirsel_core_provide_exec(HirselCoreContext *, uint64_t n, uint64_t);
/* SEND_PAIR (26): buffer borrows the first prefix. The first word at path
   is a const HirselCoreBuffer * borrowing the second prefix. Both remain
   pinned through deliver. Outcome.value counts their concatenated prefix. */
HirselCoreHostOutcome hirsel_core_host_send_pair(HirselCoreContext *, uint64_t, const HirselCoreBuffer *, const HirselCoreBuffer *, uint8_t, uint64_t, uint64_t, uint64_t);
/* TLS_CONNECT (22). Only the TLS fields in this extension are public.
   ALPN is a sequence of one-byte lengths followed by the protocol bytes.
   protocol_out points to a 255-byte buffer with length initially zero.
   peer_out points to 32 writable bytes. binding_out points to a 64-byte
   buffer with length initially zero. Success returns the stream key.
   NetError tag 9 carries the TLS case in the low byte and detail in the second.
   The input Socket is consumed on all returns. */
#define HIRSEL_CORE_TLS_CONNECT 22
typedef struct {
    uint64_t network[21];
    HirselCoreView name;
    uint8_t trust;
    HirselCoreBuffer roots;
    HirselCoreView alpn;
    HirselCoreBuffer *protocol_out;
    uint8_t *peer_out;
    HirselCoreBuffer *binding_out;
    void *runtime_state;
} HirselCoreTlsRequest;
HirselCoreHostOutcome hirsel_core_host_tls_connect(HirselCoreContext *, uint64_t, uint64_t, const HirselCoreView *, uint8_t, const HirselCoreBuffer *, const HirselCoreView *, HirselCoreBuffer *, uint8_t *, uint8_t, uint64_t, uint64_t, uint64_t);
/* SEND_SECRET (23): buffer is before, the network prefix's first word points
   to after, and secret borrows the run-owned bytes. The operation returns zero
   on success and reports the committed prefix in written on failure. */
#define HIRSEL_CORE_SEND_SECRET 23
#define HIRSEL_CORE_GRANT_SECRET 11
typedef struct {
    uint64_t network[21];
    HirselCoreBuffer secret;
} HirselCoreSendSecretRequest;
HirselCoreHostOutcome hirsel_core_host_send_secret(HirselCoreContext *, uint64_t, const HirselCoreBuffer *, const HirselCoreBuffer *, const HirselCoreBuffer *, uint8_t, uint64_t, uint64_t, uint64_t);
/* TLS_HELLO (174): reads a ClientHello on a socket a host TCP listener
   accepted. name_out points to a 253-byte buffer with length initially zero
   and receives the normalised server name, empty for none. Success returns
   the Hello's key; the Socket is consumed on every return. */
#define HIRSEL_CORE_TLS_HELLO 174
typedef struct {
    uint64_t network[21];
    HirselCoreBuffer *name_out;
    void *runtime_state;
} HirselCoreTlsHelloRequest;
HirselCoreHostOutcome hirsel_core_host_tls_hello(HirselCoreContext *, uint64_t, uint8_t, uint64_t, uint64_t, uint64_t);
/* TLS_ACCEPT (175): completes the server handshake. chain is entries of a
   u32 little-endian length and that many DER bytes; key is the Secret's
   designation, whose bytes only the provider reads; alpn is as TLS_CONNECT's.
   protocol_out receives the chosen alpn index, or UINT64_MAX for none.
   IoError.code's low byte encodes Chain as 7 and Key as 8. The Hello is
   consumed on every return. */
#define HIRSEL_CORE_TLS_ACCEPT 175
typedef struct {
    uint64_t network[21];
    HirselCoreView chain;
    uint64_t key;
    HirselCoreView alpn;
    uint64_t *protocol_out;
    void *runtime_state;
} HirselCoreTlsAcceptRequest;
HirselCoreHostOutcome hirsel_core_host_tls_accept(HirselCoreContext *, uint64_t, const HirselCoreView *, uint64_t, const HirselCoreView *, uint64_t *, uint8_t, uint64_t, uint64_t, uint64_t);
/* FORM TLS accept: the chain is a List[Bytes] and alpn a List[String];
   on success written is the chosen index plus one, or 0. */
HirselCoreHostOutcome hirsel_core_tls_accept(HirselCoreContext *, uint64_t, const HirselCoreBuffer *, uint64_t, const HirselCoreBuffer *, uint8_t, uint64_t, uint64_t, uint64_t);
/* server_name: value points to a buffer viewing the Hello's name. */
HirselCoreHostOutcome hirsel_core_tls_server_name(HirselCoreContext *, uint64_t, uint64_t);
HirselCoreHostOutcome hirsel_core_secret_seal(HirselCoreContext *, const HirselCoreBuffer *, const HirselCoreBuffer *, uint64_t);
HirselCoreHostOutcome hirsel_core_secret_narrow(HirselCoreContext *, uint64_t, const HirselCoreBuffer *, uint64_t);
#define HIRSEL_CORE_SCRAM 68
#define HIRSEL_CORE_SCRAM_VERIFY 69
HirselCoreHostOutcome hirsel_core_scram_sha256(HirselCoreContext *, uint64_t, uint64_t, const HirselCoreBuffer *, const HirselCoreBuffer *, const HirselCoreBuffer *, const HirselCoreBuffer *, uint8_t, uint64_t, uint64_t, uint64_t);
HirselCoreHostOutcome hirsel_core_scram_final_len(HirselCoreContext *, uint64_t, const HirselCoreBuffer *, const HirselCoreBuffer *, uint64_t);
HirselCoreHostOutcome hirsel_core_scram_verify(HirselCoreContext *, uint64_t, const HirselCoreBuffer *, uint64_t);
#define HIRSEL_CORE_SECRET_MAC 39
#define HIRSEL_CORE_SECRET_KDF 79
HirselCoreHostOutcome hirsel_core_secret_derive_hmac_sha256(HirselCoreContext *, uint64_t, const HirselCoreBuffer *, const HirselCoreBuffer *, const HirselCoreBuffer *, uint64_t);
HirselCoreHostOutcome hirsel_core_secret_derive_digest(HirselCoreContext *, uint64_t, uint32_t, const HirselCoreBuffer *, uint32_t, const HirselCoreBuffer *, const HirselCoreBuffer *, uint64_t);
HirselCoreHostOutcome hirsel_core_secret_derive_hkdf_sha256(HirselCoreContext *, uint64_t, const HirselCoreBuffer *, const HirselCoreBuffer *, uint64_t, const HirselCoreBuffer *, uint64_t);
HirselCoreHostOutcome hirsel_core_secret_derive_argon2id(HirselCoreContext *, uint64_t, const HirselCoreBuffer *, uint32_t, uint32_t, uint32_t, uint64_t, const HirselCoreBuffer *, uint64_t);
HirselCoreHostOutcome hirsel_core_secret_hmac_sha1(HirselCoreContext *, uint64_t, const HirselCoreBuffer *, uint8_t *, uint64_t);
HirselCoreHostOutcome hirsel_core_secret_hmac_sha256(HirselCoreContext *, uint64_t, const HirselCoreBuffer *, uint8_t *, uint64_t);
HirselCoreHostOutcome hirsel_core_secret_hmac_sha384(HirselCoreContext *, uint64_t, const HirselCoreBuffer *, uint8_t *, uint64_t);
HirselCoreHostOutcome hirsel_core_secret_hmac_sha512(HirselCoreContext *, uint64_t, const HirselCoreBuffer *, uint8_t *, uint64_t);
HirselCoreHostOutcome hirsel_core_secret_verify_hmac_sha1(HirselCoreContext *, uint64_t, const HirselCoreBuffer *, const HirselCoreBuffer *, uint64_t);
HirselCoreHostOutcome hirsel_core_secret_verify_hmac_sha256(HirselCoreContext *, uint64_t, const HirselCoreBuffer *, const HirselCoreBuffer *, uint64_t);
HirselCoreHostOutcome hirsel_core_secret_verify_hmac_sha384(HirselCoreContext *, uint64_t, const HirselCoreBuffer *, const HirselCoreBuffer *, uint64_t);
HirselCoreHostOutcome hirsel_core_secret_verify_hmac_sha512(HirselCoreContext *, uint64_t, const HirselCoreBuffer *, const HirselCoreBuffer *, uint64_t);
HirselCoreHostOutcome hirsel_core_secret_jws_hmac_sign(HirselCoreContext *, uint64_t, const HirselCoreBuffer *, uint8_t *, uint64_t, uint64_t);
HirselCoreHostOutcome hirsel_core_secret_jws_hmac_verify(HirselCoreContext *, uint64_t, const HirselCoreBuffer *, const HirselCoreBuffer *, uint64_t, uint64_t);
/* Key operations: SECRET_MAC modes 2 wrap, 3 unwrap, 4 sign; error tag is the
   KeyError case plus one (Denied 1, Invalid 2, Tampered 3, Capacity 4). */
HirselCoreHostOutcome hirsel_core_secret_wrap(HirselCoreContext *, uint64_t, uint64_t, HirselCoreBuffer *, uint64_t);
HirselCoreHostOutcome hirsel_core_secret_unwrap(HirselCoreContext *, uint64_t, const HirselCoreBuffer *, const HirselCoreBuffer *, uint64_t);
HirselCoreHostOutcome hirsel_core_secret_sign(HirselCoreContext *, uint64_t, uint64_t, uint64_t, const HirselCoreBuffer *, HirselCoreBuffer *, uint64_t);
HirselCoreHostOutcome hirsel_core_secret_p256_key(HirselCoreContext *, uint64_t, const HirselCoreBuffer *, HirselCoreBuffer *, uint64_t);
HirselCoreHostOutcome hirsel_core_secret_key_share(HirselCoreContext *, uint64_t, uint64_t, HirselCoreBuffer *, uint64_t);
void hirsel_core_media_drop(HirselCoreContext *, uint64_t);
HirselCoreHostOutcome hirsel_core_schedule_quic_initial(HirselCoreContext *, uint64_t, const HirselCoreBuffer *, uint64_t);
HirselCoreHostOutcome hirsel_core_schedule_import_traffic(HirselCoreContext *, uint64_t, uint64_t, uint64_t, uint64_t);
HirselCoreHostOutcome hirsel_core_schedule_tls13(HirselCoreContext *, uint64_t, uint64_t, uint64_t, uint64_t, const HirselCoreBuffer *, const HirselCoreBuffer *, uint64_t);
HirselCoreHostOutcome hirsel_core_schedule_dtls12(HirselCoreContext *, uint64_t, uint64_t, uint16_t, uint64_t, const HirselCoreBuffer *, const HirselCoreBuffer *, const HirselCoreBuffer *, const HirselCoreBuffer *, uint64_t);
HirselCoreHostOutcome hirsel_core_schedule_import_srtp(HirselCoreContext *, uint64_t, uint64_t, uint64_t);
HirselCoreHostOutcome hirsel_core_schedule_srtp(HirselCoreContext *, uint64_t, uint64_t, uint64_t);
HirselCoreHostOutcome hirsel_core_schedule_application(HirselCoreContext *, uint64_t, const HirselCoreBuffer *, uint64_t);
HirselCoreHostOutcome hirsel_core_schedule_key_update(HirselCoreContext *, uint64_t, bool, uint64_t);
HirselCoreHostOutcome hirsel_core_schedule_retire(HirselCoreContext *, uint64_t, uint64_t, uint64_t);
HirselCoreHostOutcome hirsel_core_schedule_finished(HirselCoreContext *, uint64_t, uint64_t, const HirselCoreBuffer *, HirselCoreBuffer *, uint64_t);
HirselCoreHostOutcome hirsel_core_schedule_verify_finished(HirselCoreContext *, uint64_t, uint64_t, const HirselCoreBuffer *, const HirselCoreBuffer *, uint64_t);
HirselCoreHostOutcome hirsel_core_schedule_sealer(HirselCoreContext *, uint64_t, uint64_t, uint64_t, uint64_t, uint64_t);
HirselCoreHostOutcome hirsel_core_schedule_sealer_pair(HirselCoreContext *, uint64_t, uint64_t, uint64_t, uint64_t, uint64_t, uint64_t, uint64_t);
HirselCoreHostOutcome hirsel_core_schedule_space_info(HirselCoreContext *, uint64_t, uint64_t, uint64_t);
HirselCoreHostOutcome hirsel_core_schedule_import_sframe(HirselCoreContext *, uint64_t, uint64_t, uint64_t);
HirselCoreHostOutcome hirsel_core_schedule_sframe_sealer(HirselCoreContext *, uint64_t, uint64_t, uint64_t, uint64_t, uint64_t);
HirselCoreHostOutcome hirsel_core_schedule_sframe_opener(HirselCoreContext *, uint64_t, uint64_t, uint64_t, uint64_t);
HirselCoreHostOutcome hirsel_core_schedule_sframe_ratchet(HirselCoreContext *, uint64_t, uint64_t, uint64_t);
HirselCoreHostOutcome hirsel_core_schedule_opener(HirselCoreContext *, uint64_t, uint64_t, uint64_t);
HirselCoreHostOutcome hirsel_core_schedule_next_opener(HirselCoreContext *, uint64_t, uint64_t);
HirselCoreHostOutcome hirsel_core_schedule_header_mask(HirselCoreContext *, uint64_t, uint64_t, bool, uint64_t);
HirselCoreHostOutcome hirsel_core_packet_seal(HirselCoreContext *, uint64_t, uint64_t, HirselCoreBuffer *, uint64_t, uint64_t, const HirselCoreBuffer *, uint64_t);
HirselCoreHostOutcome hirsel_core_packet_seal_into(HirselCoreContext *, uint64_t, uint64_t, const HirselCoreBuffer *, uint64_t, HirselCoreBuffer *, uint64_t, const HirselCoreBuffer *, uint64_t);
HirselCoreHostOutcome hirsel_core_packet_open(HirselCoreContext *, uint64_t, uint64_t, uint64_t, HirselCoreBuffer *, uint64_t, uint64_t, const HirselCoreBuffer *, uint64_t);
HirselCoreHostOutcome hirsel_core_packet_open_into(HirselCoreContext *, uint64_t, uint64_t, uint64_t, const HirselCoreBuffer *, uint64_t, uint64_t, const HirselCoreBuffer *, HirselCoreBuffer *, uint64_t);
HirselCoreHostOutcome hirsel_core_packet_mask(HirselCoreContext *, uint64_t, const HirselCoreBuffer *, uint64_t, uint8_t *, uint64_t);
HirselCoreHostOutcome hirsel_core_secret_aead_seal(HirselCoreContext *, uint64_t, uint64_t, const HirselCoreBuffer *, const HirselCoreBuffer *, const HirselCoreBuffer *, HirselCoreBuffer *, uint64_t);
HirselCoreHostOutcome hirsel_core_secret_aead_open(HirselCoreContext *, uint64_t, uint64_t, const HirselCoreBuffer *, const HirselCoreBuffer *, const HirselCoreBuffer *, HirselCoreBuffer *, uint64_t);
HirselCoreHostOutcome hirsel_core_net_narrow_tls(HirselCoreContext *, uint64_t, const HirselCoreBuffer *, uint16_t, uint16_t, uint8_t, const HirselCoreBuffer *, uint64_t);
HirselCoreHostOutcome hirsel_core_net_narrow(HirselCoreContext *, uint64_t, uint8_t, uint8_t, uint64_t, uint64_t, uint8_t, uint8_t, uint16_t, uint16_t, uint64_t);
/* NET-18: the class of an address (value), and a provider's not_public denials (value). */
HirselCoreHostOutcome hirsel_core_net_class(HirselCoreContext *, uint8_t, uint64_t, uint64_t, uint64_t);
HirselCoreHostOutcome hirsel_core_net_denials(HirselCoreContext *, uint64_t, uint64_t);
/* NET-6: net.narrow's list. begin(net) gives a builder; each narrow_* call above adds one rule to it in place of a Net; end(builder) gives the narrowed Net or Invalid. */
HirselCoreHostOutcome hirsel_core_net_narrow_begin(HirselCoreContext *, uint64_t, uint64_t);
HirselCoreHostOutcome hirsel_core_net_narrow_end(HirselCoreContext *, uint64_t, uint64_t);
HirselCoreHostOutcome hirsel_core_net_join(HirselCoreContext *, uint64_t, uint64_t, uint64_t);
HirselCoreHostOutcome hirsel_core_net_split(HirselCoreContext *, uint64_t, uint64_t);
void hirsel_core_socket_drop(HirselCoreContext *, uint64_t);

#endif
