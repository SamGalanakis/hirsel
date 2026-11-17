#include "hirsel_core_rt_host.h"
#include <unistd.h>

/* Emit the same Task observations as the evaluator's event log. */
static void event(void *state, HirselCoreEvent e) {
    (void)state;
    if (e.kind > 4) return;
    unsigned char bytes[32] = {5, 0, 0, 0};
    uint64_t fields[] = {e.kind, e.task, e.object, e.value};
    size_t at = 4;
    for (size_t i = 0; i < 4; i++) {
        size_t width = i == 0 ? 4 : 8;
        for (size_t j = 0; j < width; j++) bytes[at++] = (unsigned char)(fields[i] >> (8 * j));
    }
    size_t sent = 0;
    while (sent < sizeof(bytes)) {
        ssize_t n = write(1, bytes + sent, sizeof(bytes) - sent);
        if (n <= 0) return;
        sent += (size_t)n;
    }
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
    return status;
}
