/* Slice 2 M3 link-wrapped allocator refusal harness (-Wl,--wrap=malloc,--wrap=exit,--wrap=_exit).
 * Proves a refusing C allocator returns HIRSEL_HOST_MEMORY (11), distinct from HIRSEL_FULL (10),
 * sends nothing, never calls exit/_exit, and leaves the runtime healthy afterwards.
 */
#define _GNU_SOURCE
#include "hirsel_embed.h"

#include <assert.h>
#include <errno.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

extern void *__real_malloc(size_t size);
extern void __real_exit(int status) __attribute__((noreturn));
extern void __real__exit(int status) __attribute__((noreturn));

extern void embed_abi_fixture_init(void);
extern void embed_abi_fixture_set_mode(int32_t mode);
extern void embed_abi_fixture_manifest_fp(uint8_t out_32[32]);

#define PROGRESS_FLOOR (262144ULL + 14ULL + 8192ULL)
#define TARGET_ALLOC_SIZE 4097ULL

static _Atomic int g_no_exit = 0;
static _Atomic int g_fail_target_malloc = 0;

void __wrap_exit(int status) {
    if (atomic_load_explicit(&g_no_exit, memory_order_acquire)) {
        const char msg[] = "FORBIDDEN exit() in embed_abi_oom\n";
        (void)write(2, msg, sizeof(msg) - 1);
        abort();
    }
    __real_exit(status);
}

void __wrap__exit(int status) {
    if (atomic_load_explicit(&g_no_exit, memory_order_acquire)) {
        const char msg[] = "FORBIDDEN _exit() in embed_abi_oom\n";
        (void)write(2, msg, sizeof(msg) - 1);
        abort();
    }
    __real__exit(status);
}

void *__wrap_malloc(size_t size) {
    if (size == TARGET_ALLOC_SIZE &&
        atomic_load_explicit(&g_fail_target_malloc, memory_order_acquire)) {
        int expected = 1;
        if (atomic_compare_exchange_strong_explicit(
                &g_fail_target_malloc, &expected, 0,
                memory_order_acq_rel, memory_order_acquire)) {
            errno = ENOMEM;
            return NULL;
        }
    }
    return __real_malloc(size);
}

static _Atomic int g_outcomes = 0;

static void on_ended(void *ctx, uint32_t code, hirsel_bytes reason) {
    (void)ctx; (void)code; (void)reason;
}
static void on_outcome(void *ctx, uint64_t call, hirsel_bytes out) {
    (void)ctx; (void)call; (void)out;
    atomic_fetch_add_explicit(&g_outcomes, 1, memory_order_acq_rel);
}
static void on_change(void *ctx, uint64_t sub, hirsel_bytes ch) {
    (void)ctx; (void)sub; (void)ch;
}
static void on_sub_ended(void *ctx, uint64_t sub, hirsel_bytes end) {
    (void)ctx; (void)sub; (void)end;
}
static void on_room_event(void *ctx, uint64_t room, hirsel_bytes ev) {
    (void)ctx; (void)room; (void)ev;
}
static void on_session_ended(void *ctx, hirsel_bytes end) {
    (void)ctx; (void)end;
}

int main(int argc, char **argv) {
    assert(argc == 2);
    const char *store = argv[1];
    embed_abi_fixture_init();
    embed_abi_fixture_set_mode(0);
    atomic_store_explicit(&g_no_exit, 1, memory_order_release);

    uint8_t fp[32];
    embed_abi_fixture_manifest_fp(fp);

    hirsel_embed_config cfg;
    memset(&cfg, 0, sizeof(cfg));
    cfg.size = (uint32_t)sizeof(cfg);
    cfg.abi = HIRSEL_EMBED_ABI;
    cfg.manifest_fp.ptr = fp;
    cfg.manifest_fp.len = 32;
    cfg.n_place = 16ULL << 20;
    cfg.client_bytes = PROGRESS_FLOOR + 16384ULL;
    cfg.start_timeout_ms = 2000;
    cfg.store_dir.ptr = (const uint8_t *)store;
    cfg.store_dir.len = strlen(store);
    cfg.ended = on_ended;

    hirsel_runtime rt = { 0 };
    assert(hirsel_embed_v1_start(&cfg, &rt) == HIRSEL_OK);

    hirsel_embed_callbacks cbs = {
        .size = (uint32_t)sizeof(cbs),
        .context = NULL,
        .outcome = on_outcome,
        .change = on_change,
        .sub_ended = on_sub_ended,
        .room_event = on_room_event,
        .session_ended = on_session_ended,
    };

    /* 1. Refused malloc during `open` credential copy -> HIRSEL_HOST_MEMORY (11) */
    uint8_t cred[TARGET_ALLOC_SIZE];
    memset(cred, 0x5A, sizeof(cred));
    hirsel_session sess_fail = { 99, 99 };
    atomic_store_explicit(&g_fail_target_malloc, 1, memory_order_release);
    hirsel_status st_open = hirsel_embed_v1_open(rt, &cbs, (hirsel_bytes){ cred, sizeof(cred) }, &sess_fail);
    assert(st_open == HIRSEL_HOST_MEMORY);
    assert(sess_fail.generation == 0 && sess_fail.session == 0);

    /* 2. Normal open succeeds */
    hirsel_session sess = { 0, 0 };
    assert(hirsel_embed_v1_open(rt, &cbs, (hirsel_bytes){ NULL, 0 }, &sess) == HIRSEL_OK);

    /* 3. Refused malloc during `call` args copy -> HIRSEL_HOST_MEMORY (11), nothing sent */
    uint8_t payload[TARGET_ALLOC_SIZE];
    memset(payload, 0xAB, sizeof(payload));
    atomic_store_explicit(&g_fail_target_malloc, 1, memory_order_release);
    hirsel_status st_call = hirsel_embed_v1_call(sess, 1, 0, (hirsel_bytes){ payload, sizeof(payload) }, 5000);
    assert(st_call == HIRSEL_HOST_MEMORY);

    /* 4. Budget overflow (> admissible pool) -> HIRSEL_FULL (10), distinct from HOST_MEMORY (11) */
    uint8_t huge[32768];
    memset(huge, 0xCD, sizeof(huge));
    assert(hirsel_embed_v1_call(sess, 2, 0, (hirsel_bytes){ huge, sizeof(huge) }, 5000) == HIRSEL_FULL);

    /* 5. After OOM refusal, id 1 was NOT consumed and succeeds cleanly once malloc works */
    assert(hirsel_embed_v1_call(sess, 1, 0, (hirsel_bytes){ payload, sizeof(payload) }, 5000) == HIRSEL_OK);
    while (atomic_load_explicit(&g_outcomes, memory_order_acquire) < 1) {
        usleep(1000);
    }

    assert(hirsel_embed_v1_close(sess) == HIRSEL_OK);
    assert(hirsel_embed_v1_stop(rt) == HIRSEL_OK);
    atomic_store_explicit(&g_no_exit, 0, memory_order_release);
    printf("EMBED_ABI_OOM_OK\n");
    return 0;
}
