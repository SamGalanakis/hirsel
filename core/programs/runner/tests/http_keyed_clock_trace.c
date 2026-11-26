/* Provider samples for the keyed replay laws. Each provided clock has a fixture ordinal. */
#include "hirsel_core_rt_host.h"
#include <inttypes.h>
#include <stddef.h>
#include <stdio.h>
#include <stdlib.h>

enum { WALL_MS = 10, MONO_NS = 11 };
static uint64_t clocks[16];
static size_t count;

HirselCoreHostOutcome __real_hirsel_core_provide_clock(HirselCoreContext *, uint64_t, uint64_t, uint64_t);
HirselCoreHostOutcome __real_hirsel_core_host_clock(HirselCoreContext *, uint8_t, uint64_t, HirselCoreBuffer *, uint8_t, uint64_t);
HirselCoreHostOutcome __real_hirsel_core_host_sleep_until(HirselCoreContext *, uint64_t, uint64_t, uint64_t);

HirselCoreHostOutcome __wrap_hirsel_core_provide_clock(
    HirselCoreContext *context, uint64_t mono, uint64_t wall, uint64_t site) {
    HirselCoreHostOutcome result = __real_hirsel_core_provide_clock(context, mono, wall, site);
    if (result.status == 0) {
        if (count == sizeof(clocks) / sizeof(clocks[0])) abort();
        clocks[count++] = result.value;
    }
    return result;
}

HirselCoreHostOutcome __wrap_hirsel_core_host_clock(
    HirselCoreContext *context, uint8_t op, uint64_t handle,
    HirselCoreBuffer *operand, uint8_t mode, uint64_t site) {
    HirselCoreHostOutcome result = __real_hirsel_core_host_clock(context, op, handle, operand, mode, site);
    if (op == WALL_MS || op == MONO_NS) {
        for (size_t clock = 0; clock < count; ++clock) {
            if (clocks[clock] == handle) {
                fprintf(stderr, "%s %zu %" PRIu64 "\n", op == WALL_MS ? "wall" : "mono", clock, result.value);
            }
        }
    }
    return result;
}

HirselCoreHostOutcome __wrap_hirsel_core_host_sleep_until(
    HirselCoreContext *context, uint64_t handle, uint64_t due, uint64_t site) {
    for (size_t clock = 0; clock < count; ++clock) {
        if (clocks[clock] == handle) {
            fprintf(stderr, "sleep %zu %" PRIu64 "\n", clock, due);
        }
    }
    return __real_hirsel_core_host_sleep_until(context, handle, due, site);
}
