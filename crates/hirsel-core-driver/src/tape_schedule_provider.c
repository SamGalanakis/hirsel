/* Engine-agreement oracle: adversarial scheduler chooser for schedule.hirsel. */
#include "hirsel_core_rt_host.h"
extern uint8_t __real_hirsel_core_run(const HirselCoreProcessConfig *, HirselCoreEntry);
extern uint8_t hirsel_core_test_run_last(const HirselCoreProcessConfig *, HirselCoreEntry);
static int in_wrap;
uint8_t __wrap_hirsel_core_run(const HirselCoreProcessConfig *cfg, HirselCoreEntry entry) {
    if (in_wrap) return __real_hirsel_core_run(cfg, entry);
    in_wrap = 1;
    uint8_t status = hirsel_core_test_run_last(cfg, entry);
    in_wrap = 0;
    return status;
}
