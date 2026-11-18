#include "hirsel_core_rt.h"
#include <stdio.h>
#include <stdlib.h>

extern HirselCoreHostOutcome __real_hirsel_core_host_send_pair(HirselCoreContext *,
    uint64_t, const HirselCoreBuffer *, const HirselCoreBuffer *, uint8_t,
    uint64_t, uint64_t, uint64_t);

/* Capture the bytes the caller presents, limited to the delivered length.
 * Replay supplies the outcome without sending anything to the socket. */
HirselCoreHostOutcome __wrap_hirsel_core_host_send_pair(HirselCoreContext *ctx,
    uint64_t key, const HirselCoreBuffer *head, const HirselCoreBuffer *body,
    uint8_t until, uint64_t clock, uint64_t deadline, uint64_t site) {
    HirselCoreHostOutcome out = __real_hirsel_core_host_send_pair(
        ctx, key, head, body, until, clock, deadline, site);
    if (out.error.tag || out.status || !out.value) return out;
    if (!until || out.value > head->len + body->len) abort();
    size_t first = out.value < head->len ? out.value : head->len;
    size_t second = out.value - first;
    if ((first && fwrite(head->data, 1, first, stderr) != first)
        || (second && fwrite(body->data, 1, second, stderr) != second)) abort();
    return out;
}
