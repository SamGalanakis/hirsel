#include "hirsel_core_rt.h"
#include <stdint.h>
#include <stdlib.h>
#include <sys/resource.h>
#include <unistd.h>

static void cleanup(void) { write(2, "CLEANUP\n", 8); }

int main(void) {
    if (atexit(cleanup)) return 90;
    const struct rlimit denied = {0, 0};
    if (setrlimit(RLIMIT_CORE, &denied) || setrlimit(RLIMIT_AS, &denied)) return 91;
    write(1, "entered\n", 8);
    /* Only engine bookkeeping: each context makes a limit domain, its slot
       page/inbox and the Context box. No language/payload allocation occurs.
       Keep domains live until System cannot extend its already-mapped arena. */
    for (size_t n = 0; n < 1000000; ++n) {
        HirselCoreContext *volatile context = hirsel_core_context_new(UINT64_MAX);
        (void)context;
    }
    return 92;
}
