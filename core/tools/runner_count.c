/* L3's native count (RUNNER-SPEC v2.6): linked through
 * --wrap=hirsel_core_os_run_with_sources into the test executable of one case, it counts
 * CONC-10 TRY_SEND events (the case's body, once per run, and the publisher, once per
 * answer it tells) and fails the run (status 105) unless there are exactly TRY_SENDS. */
#include "hirsel_core_rt_host.h"
#include <stdio.h>
#ifndef TRY_SENDS
#error "define TRY_SENDS"
#endif
static uint64_t try_sends;
#ifdef HUNG_BODY_SENDS
static struct { uint64_t object, task, count; int before_slice; } senders[3];
static size_t sender_count;
static uint64_t slice_task, slices;
static int too_many_senders;
#endif
static void event(void *state, HirselCoreEvent e) {
    (void)state;
    if (e.kind == HIRSEL_CORE_EVENT_TRY_SEND) {
        try_sends++;
#ifdef HUNG_BODY_SENDS
        size_t i = 0;
        while (i < sender_count && senders[i].object != e.object) i++;
        if (i == 3) { too_many_senders = 1; return; }
        if (i == sender_count) {
            sender_count++;
            senders[i].object = e.object;
            senders[i].task = e.task;
            senders[i].before_slice = slices == 0;
        }
        senders[i].count++;
#endif
    }
#ifdef HUNG_BODY_SENDS
    if (e.kind == HIRSEL_CORE_EVENT_STEP_SLICE) { slice_task = e.task; slices++; }
#endif
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
#ifdef HUNG_BODY_SENDS
    if (too_many_senders || sender_count != 3 || slices != 1) return 105;
    size_t body = 3, marker = 3, told = 3;
    for (size_t k = 0; k < 3; k++) {
        if (senders[k].count == HUNG_BODY_SENDS) body = k;
        else if (senders[k].count == 1) marker = k;
        else if (senders[k].count == HUNG_TOLDS) told = k;
    }
    if (body == 3 || marker == 3 || told == 3 ||
        senders[body].task != slice_task || senders[marker].task == slice_task ||
        senders[told].task == slice_task || !senders[marker].before_slice) return 105;
#endif
    return try_sends == TRY_SENDS ? 0 : 105;
}
