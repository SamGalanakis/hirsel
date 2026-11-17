/* L20's native mark (RUNNER-SPEC v2.11 §8): linked through
 * --wrap=hirsel_core_os_run_with_sources into runner_cover.hirsel's program on the
 * charge-observation runtime, it records the charge checkpoint "subscribed" at the first
 * CHECK_CANCEL event, the program's task.check_cancel() after its initial Batches, and
 * fails the run (status 105) unless there was exactly one. */
#include "hirsel_core_rt.h"
#include "hirsel_core_rt_host.h"
static unsigned marks;
static void event(void *state, HirselCoreEvent e) {
    (void)state;
    if (e.kind == HIRSEL_CORE_EVENT_CHECK_CANCEL && marks++ == 0)
        hirsel_core_memory_checkpoint((const uint8_t *)"subscribed", 10);
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
    if (status) return status;
    return marks == 1 ? 0 : 105;
}
