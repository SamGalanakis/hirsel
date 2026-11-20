#include "hirsel_core_rt_host.h"
#include <stdatomic.h>
#include <stdio.h>
#include <stdlib.h>

static _Atomic unsigned calls;
static const HirselCoreHostOutcome refused = {.error = {.tag = 1}};

static HirselCoreWait refuse_wait(void *state, HirselCoreHostRequest *r, uint64_t token) {
    ++calls;
    return (HirselCoreWait){0};
}

static HirselCoreStep refuse_step(void *state, HirselCoreHostRequest *r, uint64_t token) {
    ++calls;
    return (HirselCoreStep){.outcome = refused};
}

static HirselCoreHostOutcome refuse_abort(void *state, HirselCoreHostRequest *r,
    uint64_t token, uint8_t reason) {
    ++calls;
    return refused;
}

static HirselCoreHostOutcome refuse_deliver(void *state, HirselCoreHostRequest *r,
    HirselCoreHostOutcome out) {
    ++calls;
    return refused;
}

extern uint8_t __real_hirsel_core_run(const HirselCoreProcessConfig *, HirselCoreEntry);

uint8_t __wrap_hirsel_core_run(const HirselCoreProcessConfig *config, HirselCoreEntry entry) {
    if (!getenv("HIRSEL_TL9_REPLAY")) return __real_hirsel_core_run(config, entry);
    HirselCoreProvider provider = {
        .wait = refuse_wait, .issue = refuse_step, .resume = refuse_step,
        .abort = refuse_abort, .deliver = refuse_deliver
    };
    HirselCoreProcessConfig replay = *config;
    replay.provider = &provider;
    uint8_t status = __real_hirsel_core_run(&replay, entry);
    fprintf(stderr, "inner calls: %u\n", (unsigned)calls);
    return status;
}

extern HirselCoreHostOutcome __real_hirsel_core_host_io(HirselCoreContext *,
    uint8_t, uint64_t, HirselCoreBuffer *, uint8_t, uint64_t);

/* Observe program output even when replay suppresses the live Stdout effect. */
HirselCoreHostOutcome __wrap_hirsel_core_host_io(HirselCoreContext *ctx, uint8_t op,
    uint64_t handle, HirselCoreBuffer *buf, uint8_t mode, uint64_t site) {
    HirselCoreHostOutcome out = __real_hirsel_core_host_io(ctx, op, handle, buf, mode, site);
    if (op == 12 && !out.status && !out.error.tag
        && fwrite(buf->data, 1, buf->len, stderr) != buf->len) abort();
    return out;
}
