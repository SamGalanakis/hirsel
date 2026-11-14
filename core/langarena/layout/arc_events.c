/* Separate event-recording configuration. Wrap the existing driver process config;
 * the callback records the actual source program's Arc schedule, with no source hook. */
#include "hirsel_core_rt.h"
#include "hirsel_core_rt_host.h"
#include "hirsel_core_rt_sites.h"
#include <assert.h>
#include <inttypes.h>
#include <stdio.h>
static uint64_t events[256], last_releases;
static void event(void *state, HirselCoreEvent value) {
    (void)state; events[value.kind]++;
    if (value.kind == HIRSEL_CORE_ARC_EVENT_RELEASE && value.value == 1) last_releases++;
}
uint8_t __wrap_hirsel_core_os_run_with_sources(const HirselCoreOsConfig *source,
                                             const HirselCoreView *names,
                                             const HirselCoreSourceTable *sources,
                                             HirselCoreEntry entry) {
    (void)names;
    /* This harness is only for the no-Dir Arc phase. Use the public constructor
       and process API, retaining the original grants, root allowance and argv. */
    assert(source->directories_len == 0);
    HirselCoreOpenProcess opened = hirsel_core_os_process_new(source);
    assert(opened.process);
    hirsel_core_os_process_set_sources(opened.process, sources);
    HirselCoreProcessConfig cfg = *hirsel_core_os_process_config(opened.process);
    cfg.scheduler.event = event;
    uint8_t status = hirsel_core_run(&cfg, entry);
    hirsel_core_os_process_free(opened.process);
    /* 64 initial +256*33 new nodes, two shares/version, every holder released. */
    assert(events[10] == 8512 && events[11] == 512 && events[12] == 9024 && events[13] == 0);
    assert(last_releases == 8512);
    fprintf(stderr, "Arc events new=%" PRIu64 " share=%" PRIu64 " release=%" PRIu64 " last=%" PRIu64 "\n",
            events[10], events[11], events[12], last_releases);
    return status;
}
