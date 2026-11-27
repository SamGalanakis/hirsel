#include <mach/mach.h>
#include <pthread.h>
#include <string.h>

#include "AbiTests.h"

uint32_t hirsel_gate_workers(void) {
    thread_act_array_t threads;
    mach_msg_type_number_t count;
    if (task_threads(mach_task_self(), &threads, &count) != KERN_SUCCESS) return UINT32_MAX;

    uint32_t workers = 0;
    for (mach_msg_type_number_t i = 0; i < count; ++i) {
        pthread_t thread = pthread_from_mach_thread_np(threads[i]);
        char name[64];
        if (thread && pthread_getname_np(thread, name, sizeof name) == 0) {
            if (!strcmp(name, "hirsel-embed-place") || !strcmp(name, "hirsel-embed-worker") ||
                !strcmp(name, "hirsel-unit") || !strncmp(name, "hirsel-w", 8)) {
                ++workers;
            }
        }
        mach_port_deallocate(mach_task_self(), threads[i]);
    }
    vm_deallocate(mach_task_self(), (vm_address_t)threads, count * sizeof(*threads));
    return workers;
}
