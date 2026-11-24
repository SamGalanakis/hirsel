/* L15 uses the normal scheduler. Count the sends on each body-local channel. */
#include "hirsel_core_rt.h"
#include "hirsel_core_rt_host.h"
#include "hirsel_core_rt_sites.h"
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>

enum { BODY_SENDS = 37, OBJECTS = 65536 };
static uint32_t sends[OBJECTS];

static void observe(void *state, HirselCoreEvent event) {
    (void)state;
    if (event.kind != HIRSEL_CORE_EVENT_TRY_SEND) return;
    if (event.object >= OBJECTS) abort();
    sends[event.object]++;
}

uint8_t __wrap_hirsel_core_os_run_with_sources(const HirselCoreOsConfig *source,
                                             const HirselCoreView *names,
                                             const HirselCoreSourceTable *sources,
                                             HirselCoreEntry entry) {
    (void)names;
    HirselCoreOpenProcess opened = hirsel_core_os_process_new(source);
    if (!opened.process) return 103;
    hirsel_core_os_process_set_sources(opened.process, sources);
    HirselCoreProcessConfig config = *hirsel_core_os_process_config(opened.process);
    config.scheduler.event = observe;
    uint8_t status = hirsel_core_run(&config, entry);
    hirsel_core_os_process_free(opened.process);
    if (status) return status;
    unsigned runs = 0;
    for (size_t i = 0; i < OBJECTS; i++) runs += sends[i] == BODY_SENDS;
    fprintf(stderr, "L15 body runs: %u\n", runs);
    const char *expected = getenv("HIRSEL_L15_BODY_RUNS");
    return expected && runs == strtoul(expected, NULL, 10) ? 0 : 1;
}
