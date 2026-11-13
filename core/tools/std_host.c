/* Scripted partial stream host for native std contracts, linked through --wrap.
 * The first Stdout/err row commits FIRST bytes then reports EIO; later rows use
 * the production provider. This can stop inside a UTF8 scalar. */
#include "hirsel_core_rt_host.h"
#include <unistd.h>
#include <stdlib.h>
#ifndef FIRST
#define FIRST 1
#endif
extern HirselCoreHostOutcome __real_hirsel_core_host_op(
    HirselCoreContext *, uint8_t, uint64_t, HirselCoreBuffer *, uint8_t, uint64_t);
HirselCoreHostOutcome __wrap_hirsel_core_host_op(
    HirselCoreContext *ctx, uint8_t op, uint64_t handle, HirselCoreBuffer *b,
    uint8_t mode, uint64_t site) {
    static unsigned calls;
    if ((op == 12 || op == 13) && calls++ == 0) {
        size_t n = FIRST < b->len ? FIRST : b->len;
        if (n && write(op == 12 ? 1 : 2, b->data, n) != (ssize_t)n) _exit(103);
        HirselCoreHostOutcome out = {0};
        out.error.tag = 5; out.error.code = 5; out.written = n;
        return out;
    }
    return __real_hirsel_core_host_op(ctx, op, handle, b, mode, site);
}
