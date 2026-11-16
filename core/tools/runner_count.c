/* L3's native count (RUNNER-SPEC v2.6): linked through
 * --wrap=hirsel_core_os_run_with_sources into the test executable of one case, it counts
 * CONC-10 TRY_SEND events (the case's body is the only caller of try_send, once per run)
 * and fails the run (status 105) unless there are exactly TRY_SENDS. */
#include "hirsel_core_rt_host.h"
#include <stdio.h>
#ifndef TRY_SENDS
#error "define TRY_SENDS"
#endif
static uint64_t try_sends;
static void event(void *state, HirselCoreEvent e) {
    (void)state;
    if (e.kind == HIRSEL_CORE_EVENT_TRY_SEND) try_sends++;
}
uint8_t __wrap_hirsel_core_os_run_with_sources(const HirselCoreOsConfig *input,
    const HirselCoreView *names, const HirselCoreSourceTable *sources, HirselCoreEntry entry) {
    (void)names;
    HirselCoreOpenProcess open = hirsel_core_os_process_new(input);
    if (!open.process) return HIRSEL_CORE_NOT_STARTED;
    hirsel_core_os_process_set_sources(open.process, sources);
    HirselCoreProcessConfig cfg = *hirsel_core_os_process_config(open.process);
    cfg.scheduler.event = event;
    uint8_t status = hirsel_core_run(&cfg, entry);
    hirsel_core_os_process_free(open.process);
    dprintf(2, "runner_count: try_send %llu, want %llu\n", (unsigned long long)try_sends,
            (unsigned long long)TRY_SENDS);
    if (status) return status;
    return try_sends == TRY_SENDS ? 0 : 105;
}
