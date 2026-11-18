/* Scripted schedules for native std contracts, linked through
 * --wrap=hirsel_core_os_run_with_sources. The law runs once per seed 1..SEEDS, each
 * run a fresh OS process configuration with a seeded ScheduleScript. The first failing run's status is returned. After all runs pass,
 * the shim refuses (exit 104) when fewer than two seeds produced different event
 * streams, so a law cannot pass vacuously under one schedule, and when any run emitted
 * more than MAX_EVENTS CONC-10 events. */
#include "hirsel_core_rt_host.h"
#include <stdio.h>
#include <unistd.h>
#ifndef SEEDS
#define SEEDS 32
#endif
#ifndef MAX_EVENTS
#define MAX_EVENTS UINT64_MAX
#endif
extern uint8_t hirsel_core_test_run_scripted(const HirselCoreProcessConfig *, HirselCoreEntry, uint64_t);
static struct { uint64_t hash, events; } s;
static void event(void *state, HirselCoreEvent e) {
    (void)state;
    uint64_t words[4] = {e.kind, e.task, e.object, e.value};
    for (int i = 0; i < 4; i++) s.hash = (s.hash ^ words[i]) * 1099511628211u;
    s.events++;
}
uint8_t __wrap_hirsel_core_os_run_with_sources(const HirselCoreOsConfig *input,
    const HirselCoreView *names, const HirselCoreSourceTable *sources, HirselCoreEntry entry) {
    (void)names;
    uint64_t first = 0, distinct = 0;
    for (uint64_t seed = 1; seed <= SEEDS; seed++) {
        HirselCoreOpenProcess open = hirsel_core_os_process_new(input);
        if (!open.process) return HIRSEL_CORE_NOT_STARTED;
        hirsel_core_os_process_set_sources(open.process, sources);
        HirselCoreProcessConfig cfg = *hirsel_core_os_process_config(open.process);
        s.hash = 14695981039346656037u; s.events = 0;
        cfg.scheduler.event = event;
        uint8_t status = hirsel_core_test_run_scripted(&cfg, entry, seed);
        hirsel_core_os_process_free(open.process);
        if (status) {
            dprintf(2, "schedule seed %llu: status %u\n", (unsigned long long)seed, status);
            return status;
        }
        if (s.events > MAX_EVENTS) {
            dprintf(2, "schedule seed %llu: %llu events, more than %llu\n", (unsigned long long)seed,
                    (unsigned long long)s.events, (unsigned long long)MAX_EVENTS);
            return 104;
        }
        if (seed == 1) first = s.hash;
        else if (s.hash != first) distinct++;
    }
    if (SEEDS > 1 && !distinct) {
        dprintf(2, "schedule: all %d seeds produced one event stream\n", SEEDS);
        return 104;
    }
    return 0;
}
