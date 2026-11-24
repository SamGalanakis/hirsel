#include "hirsel_core_rt_host.h"
#include <inttypes.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

HirselCoreHostOutcome __real_hirsel_core_host_io(
    HirselCoreContext *, uint8_t, uint64_t, HirselCoreBuffer *, uint8_t, uint64_t);

#ifdef COUNT_ALLOCATIONS
uint64_t hirsel_bench_allocation_count(void);

static void count(const char *phase) {
    char line[64];
    int n = snprintf(line, sizeof line, "BENCH_ALLOCATIONS %s %" PRIu64 "\n",
                     phase, hirsel_bench_allocation_count());
    if (n < 0 || (size_t)n >= sizeof line || write(STDERR_FILENO, line, (size_t)n) != n)
        _exit(1);
}
#else
static void count(const char *phase) { (void)phase; }
#endif

/* Handle the two markers before the runtime dispatches stdout to a helper.
 * Only this benchmark is linked with the wrapper. Its gate and result are
 * outside the packet allocator interval, including the runtime's own I/O. */
HirselCoreHostOutcome __wrap_hirsel_core_host_io(
    HirselCoreContext *context, uint8_t op, uint64_t handle,
    HirselCoreBuffer *buf, uint8_t mode, uint64_t site) {
    if (op != 12 || buf == NULL)
        return __real_hirsel_core_host_io(context, op, handle, buf, mode, site);
    const int ready = buf->len == 6 && memcmp(buf->data, "READY\n", 6) == 0;
    const int done = buf->len >= 5 && memcmp(buf->data, "DONE ", 5) == 0;
    if (!ready && !done)
        return __real_hirsel_core_host_io(context, op, handle, buf, mode, site);

    if (done)
        count("DONE");
    char gate;
    if (write(STDOUT_FILENO, buf->data, buf->len) != (ssize_t)buf->len
        || read(STDIN_FILENO, &gate, 1) != 1 || gate != 'x')
        _exit(1);
    if (ready)
        count("READY");
    return (HirselCoreHostOutcome){.written = buf->len};
}
