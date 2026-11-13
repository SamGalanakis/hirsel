/* Scripted partial stream host for native std contracts, linked through --wrap.
 * The first Stdout/err row commits FIRST bytes then reports EIO; later rows use
 * the production provider. This can stop inside a UTF8 scalar. */
#include "hirsel_core_rt_host.h"
#include <unistd.h>
#include <stdlib.h>
#include <string.h>
#ifndef FIRST
#define FIRST 1
#endif
#ifndef FIRST_OP
#define FIRST_OP 0
#endif
extern HirselCoreHostOutcome __real_hirsel_core_host_op(
    HirselCoreContext *, uint8_t, uint64_t, HirselCoreBuffer *, uint8_t, uint64_t);
HirselCoreHostOutcome __wrap_hirsel_core_host_op(
    HirselCoreContext *ctx, uint8_t op, uint64_t handle, HirselCoreBuffer *b,
    uint8_t mode, uint64_t site) {
    static unsigned calls;
    if (((FIRST_OP == 0 && (op == 12 || op == 13)) || FIRST_OP == op) && calls++ == 0) {
        size_t n = FIRST < b->len ? FIRST : b->len;
        if (op == 9) {
            /* Script: remove exactly child "a", then fail. The production
             * provider still applies the ordinary path and authority rules. */
            uint8_t path[512];
            if (b->len + 2 > sizeof path) _exit(103);
            memcpy(path, b->data, b->len); memcpy(path + b->len, "/a", 2);
            HirselCoreBuffer child = {path, b->len + 2, sizeof path};
            HirselCoreHostOutcome removed = __real_hirsel_core_host_op(ctx, 8, handle, &child, 0, site);
            if (removed.error.tag || removed.status) _exit(103);
            n = 0;
        } else if (op == 2) {
            HirselCoreBuffer prefix = {b->data, n, n};
            HirselCoreHostOutcome wrote = __real_hirsel_core_host_op(ctx, op, handle, &prefix, mode, site);
            if (wrote.error.tag || wrote.status) _exit(103);
            n = wrote.written;
        } else if (n && write(op == 12 ? 1 : 2, b->data, n) != (ssize_t)n) _exit(103);
        HirselCoreHostOutcome out = {0};
        out.error.tag = 5; out.error.code = 5; out.written = n;
        return out;
    }
    return __real_hirsel_core_host_op(ctx, op, handle, b, mode, site);
}
