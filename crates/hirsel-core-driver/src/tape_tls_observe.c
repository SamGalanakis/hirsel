#include "hirsel_core_rt_host.h"
#include <stdio.h>
#include <stdlib.h>

extern size_t hirsel_core_test_replay_provider_calls(void);
extern uint8_t __real_hirsel_core_os_replay(const HirselCoreOsConfig *, const HirselCoreView *,
    const HirselCoreSourceTable *, HirselCoreEntry, const char *, uint64_t, const char *,
    const uint8_t revision[32]);

uint8_t __wrap_hirsel_core_os_replay(const HirselCoreOsConfig *config, const HirselCoreView *names,
    const HirselCoreSourceTable *sources, HirselCoreEntry entry, const char *path, uint64_t mode,
    const char *identity, const uint8_t revision[32]) {
    uint8_t status = __real_hirsel_core_os_replay(config, names, sources, entry, path, mode,
        identity, revision);
    fprintf(stderr, "inner calls: %zu\n", hirsel_core_test_replay_provider_calls());
    return status;
}

extern HirselCoreHostOutcome __real_hirsel_core_host_io(HirselCoreContext *,
    uint8_t, uint64_t, HirselCoreBuffer *, uint8_t, uint64_t);

/* Observe program output even when replay suppresses the live Stdout effect. */
HirselCoreHostOutcome __wrap_hirsel_core_host_io(HirselCoreContext *ctx, uint8_t op,
    uint64_t handle, HirselCoreBuffer *buf, uint8_t mode, uint64_t site) {
    HirselCoreHostOutcome out = __real_hirsel_core_host_io(ctx, op, handle, buf, mode, site);
    if (op == 12 && !out.status && !out.error.tag
        && fwrite(buf->data, 1, buf->len, stderr) != buf->len) abort();
    return out;
}
