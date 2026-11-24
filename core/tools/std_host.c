/* Scripted partial stream host for native std contracts, linked through --wrap.
 * The first Stdout/err row commits FIRST bytes then reports EIO; later rows use
 * the production provider. This can stop inside a UTF8 scalar. */
#include "hirsel_core_rt_host.h"
#include <unistd.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#ifndef FIRST
#define FIRST 1
#endif
#ifndef FIRST_OP
#define FIRST_OP 0
#endif
static HirselCoreHostOutcome interrupted(size_t written) {
    HirselCoreHostOutcome out = {0};
    out.error.tag = 5;
    out.error.code = 5;
    out.written = written;
    return out;
}

#if FIRST_OP == 0
extern HirselCoreHostOutcome __real_hirsel_core_host_io(
    HirselCoreContext *, uint8_t, uint64_t, HirselCoreBuffer *, uint8_t, uint64_t);
HirselCoreHostOutcome __wrap_hirsel_core_host_io(
    HirselCoreContext *ctx, uint8_t op, uint64_t handle, HirselCoreBuffer *b,
    uint8_t mode, uint64_t site) {
    static unsigned calls;
    if ((op == 12 || op == 13) && calls++ == 0) {
        size_t n = FIRST < b->len ? FIRST : b->len;
        if (n && write(op == 12 ? 1 : 2, b->data, n) != (ssize_t)n) _exit(103);
        return interrupted(n);
    }
    return __real_hirsel_core_host_io(ctx, op, handle, b, mode, site);
}
#elif FIRST_OP == 44
extern HirselCoreHostOutcome __real_hirsel_core_host_op(
    HirselCoreContext *, uint8_t, uint64_t, HirselCoreBuffer *, uint8_t, uint64_t);

static int path_is(HirselCoreBuffer *b, const char *path) {
    return b && b->len == strlen(path) && memcmp(b->data, path, b->len) == 0;
}

HirselCoreHostOutcome __wrap_hirsel_core_host_op(
    HirselCoreContext *ctx, uint8_t op, uint64_t handle, HirselCoreBuffer *b,
    uint8_t mode, uint64_t site) {
    if (op == 44 && !path_is(b, "base") && !path_is(b, "base/good") &&
        !path_is(b, "base/good/deep") && !path_is(b, "missing")) {
        _exit(103);
    }
    return __real_hirsel_core_host_op(ctx, op, handle, b, mode, site);
}
#elif FIRST_OP == 119
enum { Write = 2, Close = 3, Remove = 8, Rename = 14, Sync = 16, SyncDir = 17, Stat = 46, Mkstemp = 119 };
static const uint8_t primary_faults[] = {
    Mkstemp, Write, Sync, Close, Rename, SyncDir, Rename, 0, Write, Write, 0
};
static unsigned attempt;
static uint8_t fault;
static uint64_t probe_dir;
static const char *probe_name;
static int saw_target;
static _Thread_local int pin_entropy;

extern HirselCoreHostOutcome __real_hirsel_core_host_op(
    HirselCoreContext *, uint8_t, uint64_t, HirselCoreBuffer *, uint8_t, uint64_t);
extern HirselCoreHostOutcome __real_hirsel_core_host_rename(
    HirselCoreContext *, uint64_t, HirselCoreBuffer *, HirselCoreBuffer *, uint64_t);
extern ssize_t __real_getrandom(void *, size_t, unsigned);

ssize_t __wrap_getrandom(void *buffer, size_t len, unsigned flags) {
    if (pin_entropy) {
        memset(buffer, 0xaa, len);
        return (ssize_t)len;
    }
    return __real_getrandom(buffer, len, flags);
}

static void observe_before_rename(HirselCoreContext *ctx, uint64_t site, const char *phase) {
    if (!probe_name) return;
    HirselCoreBuffer target = {(uint8_t *)probe_name, strlen(probe_name), 0};
    HirselCoreHostOutcome stat = __real_hirsel_core_host_op(ctx, Stat, probe_dir, &target, 0, site);
    if (stat.status || stat.error.tag > 1) _exit(103);
    if (!stat.error.tag) {
        saw_target = 1;
        fprintf(stderr, "replace exposed %llu destination bytes %s before rename\n",
                (unsigned long long)stat.value, phase);
    }
}

HirselCoreHostOutcome __wrap_hirsel_core_host_op(
    HirselCoreContext *ctx, uint8_t op, uint64_t handle, HirselCoreBuffer *b,
    uint8_t mode, uint64_t site) {
    if (op == Mkstemp) {
        if (attempt >= sizeof primary_faults) _exit(103);
        fault = primary_faults[attempt++];
        if (attempt == 9) probe_name = ".hirsel-tmp-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        if (attempt >= 10) probe_name = ".hirsel-tmp-0aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        if (probe_name) {
            probe_dir = handle;
            observe_before_rename(ctx, site, "before create");
            pin_entropy = 1;
            HirselCoreHostOutcome out = __real_hirsel_core_host_op(ctx, op, handle, b, mode, site);
            pin_entropy = 0;
            observe_before_rename(ctx, site, "after create");
            return out;
        }
    }
    if (!fault || op != fault)
        return __real_hirsel_core_host_op(ctx, op, handle, b, mode, site);
    fault = 0;
    if (op == Write) {
        /* A committed prefix in the temp file must never reach the target. */
        if (b->len < 2) _exit(103);
        HirselCoreBuffer prefix = {b->data, 2, 2};
        HirselCoreHostOutcome out = __real_hirsel_core_host_op(ctx, op, handle, &prefix, mode, site);
        if (out.error.tag || out.status || out.written != 2) _exit(103);
        observe_before_rename(ctx, site, "after partial write");
        if (saw_target) _exit(104);
        return interrupted(out.written);
    }
    if (op == Close) {
        /* A failed close still consumes the handle. */
        HirselCoreHostOutcome out = __real_hirsel_core_host_op(ctx, op, handle, b, mode, site);
        if (out.error.tag || out.status) _exit(103);
    }
    return interrupted(0);
}

HirselCoreHostOutcome __wrap_hirsel_core_host_rename(
    HirselCoreContext *ctx, uint64_t handle, HirselCoreBuffer *from, HirselCoreBuffer *to,
    uint64_t site) {
    observe_before_rename(ctx, site, "after full write");
    if (saw_target) _exit(104);
    probe_name = NULL;
    if (fault != Rename)
        return __real_hirsel_core_host_rename(ctx, handle, from, to, site);
    fault = attempt == 7 ? Remove : 0;
    return interrupted(0);
}
#else
extern HirselCoreHostOutcome __real_hirsel_core_host_op(
    HirselCoreContext *, uint8_t, uint64_t, HirselCoreBuffer *, uint8_t, uint64_t);
HirselCoreHostOutcome __wrap_hirsel_core_host_op(
    HirselCoreContext *ctx, uint8_t op, uint64_t handle, HirselCoreBuffer *b,
    uint8_t mode, uint64_t site) {
    static unsigned calls;
    if (FIRST_OP == op && calls++ == 0) {
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
        }
        return interrupted(n);
    }
    return __real_hirsel_core_host_op(ctx, op, handle, b, mode, site);
}
#endif
