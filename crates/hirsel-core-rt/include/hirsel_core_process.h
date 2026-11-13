#ifndef HIRSEL_CORE_PROCESS_H
#define HIRSEL_CORE_PROCESS_H
#include "hirsel_core_rt.h"
/* Provider operations: Open 0, Read 1, Write 2, Close 3, TryLock 4, Unlock 5,
   Mkdir 6, Mkdtemp 7, Remove 8, RemoveTree 9, WallMs 10, MonoNs 11,
   Stdout 12, Stderr 13. Open modes: Read 0, Write 1, Append 2, CreateNew 3. */
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
typedef struct {
    void *state;
    HirselCoreHostOutcome (*issue)(void *, HirselCoreHostRequest *);
    HirselCoreHostOutcome (*deliver)(void *, HirselCoreHostRequest *, HirselCoreHostOutcome);
} HirselCoreProvider;
HirselCoreHostOutcome hirsel_core_host(HirselCoreContext *, const HirselCoreProvider *, HirselCoreHostRequest *);
typedef struct { const uint8_t *data; size_t len; } HirselCoreView;
typedef struct { uint8_t kind, exit; } HirselCoreMainResult;
typedef struct HirselCoreProcessConfig {
    uint64_t root_limit;
    const HirselCoreView *args;
    size_t args_len;
    const uint64_t *grants;
    size_t grants_len;
    const HirselCoreProvider *provider;
    void *state;
    void (*diagnostic)(void *, const uint8_t *, size_t);
    HirselCoreView (*describe)(void *, const HirselCoreCause *);
} HirselCoreProcessConfig;
typedef uint8_t (*HirselCoreEntry)(HirselCoreContext *, const HirselCoreProcessConfig *, HirselCoreMainResult *);
uint8_t hirsel_core_run(const HirselCoreProcessConfig *, HirselCoreEntry);
uint64_t hirsel_core_args_len(const HirselCoreProcessConfig *);
typedef struct { HirselCoreView value; bool present; } HirselCoreArgument;
HirselCoreArgument hirsel_core_args_get(const HirselCoreProcessConfig *, uint64_t);
uint8_t hirsel_core_check_cancel(HirselCoreContext *, uint64_t site);
#endif
