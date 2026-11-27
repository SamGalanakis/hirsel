/* Slice 2 ASan C harness covering B1, B2, and M13 against `hirsel_embed.h`. */
#define _GNU_SOURCE
#include "hirsel_embed.h"

#include <assert.h>
#include <pthread.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

extern void embed_abi_fixture_init(void);
extern void embed_abi_fixture_set_mode(int32_t mode);
extern void embed_abi_fixture_manifest_fp(uint8_t out_32[32]);

#define PROGRESS_FLOOR (262144ULL + 14ULL + 8192ULL)

typedef struct {
    uint64_t magic;
    _Atomic int ended_count;
    _Atomic uint32_t end_code;
    int free_on_end;
} runtime_ctx;

typedef struct {
    uint64_t magic;
    _Atomic int outcomes;
    _Atomic int session_ends;
    _Atomic int self_close_mode;
    _Atomic int busy_rc;
    _Atomic int deferred_rc;
    _Atomic int losing_rc;
    hirsel_runtime rt;
    hirsel_session sess;
    pthread_mutex_t mu;
    pthread_cond_t cv;
    int hold_in_outcome;
    int in_outcome;
    int release_outcome;
    int free_on_end;
} session_ctx;

static void on_runtime_ended(void *ctx, uint32_t code, hirsel_bytes reason) {
    (void)reason;
    runtime_ctx *r = (runtime_ctx *)ctx;
    if (!r) return;
    assert(r->magic == 0xC0FFEE1234567890ULL);
    atomic_store_explicit(&r->end_code, code, memory_order_release);
    atomic_fetch_add_explicit(&r->ended_count, 1, memory_order_acq_rel);
    if (r->free_on_end) {
        r->magic = 0xDEADBEEFDEADBEEFULL;
        free(r);
    }
}

static void on_outcome(void *ctx, uint64_t call, hirsel_bytes outcome) {
    (void)call;
    (void)outcome;
    session_ctx *s = (session_ctx *)ctx;
    assert(s->magic == 0x5E55104012345678ULL);
    if (atomic_load_explicit(&s->self_close_mode, memory_order_acquire)) {
        atomic_store_explicit(&s->busy_rc, hirsel_embed_v1_stop(s->rt), memory_order_release);
        atomic_store_explicit(&s->deferred_rc, hirsel_embed_v1_close(s->sess), memory_order_release);
        atomic_store_explicit(&s->losing_rc, hirsel_embed_v1_close(s->sess), memory_order_release);
    }
    if (s->hold_in_outcome) {
        pthread_mutex_lock(&s->mu);
        s->in_outcome = 1;
        pthread_cond_broadcast(&s->cv);
        while (!s->release_outcome) {
            pthread_cond_wait(&s->cv, &s->mu);
        }
        pthread_mutex_unlock(&s->mu);
    }
    atomic_fetch_add_explicit(&s->outcomes, 1, memory_order_acq_rel);
}

static void on_change(void *ctx, uint64_t sub, hirsel_bytes change) {
    (void)ctx; (void)sub; (void)change;
}
static void on_sub_ended(void *ctx, uint64_t sub, hirsel_bytes end) {
    (void)ctx; (void)sub; (void)end;
}
static void on_room_event(void *ctx, uint64_t room, hirsel_bytes event) {
    (void)ctx; (void)room; (void)event;
}
static void on_session_ended(void *ctx, hirsel_bytes end) {
    (void)end;
    session_ctx *s = (session_ctx *)ctx;
    assert(s->magic == 0x5E55104012345678ULL);
    atomic_fetch_add_explicit(&s->session_ends, 1, memory_order_acq_rel);
    if (s->free_on_end) {
        s->magic = 0xDEADBEEFDEADBEEFULL;
        pthread_mutex_destroy(&s->mu);
        pthread_cond_destroy(&s->cv);
        free(s);
    }
}

static hirsel_embed_config make_cfg(const uint8_t fp[32], const char *store, runtime_ctx *r) {
    hirsel_embed_config c;
    memset(&c, 0, sizeof(c));
    c.size = (uint32_t)sizeof(c);
    c.abi = HIRSEL_EMBED_ABI;
    c.manifest_fp.ptr = fp;
    c.manifest_fp.len = 32;
    c.n_place = 16ULL << 20;
    c.client_bytes = PROGRESS_FLOOR + 65536ULL;
    c.start_timeout_ms = 2000;
    c.store_dir.ptr = (const uint8_t *)store;
    c.store_dir.len = strlen(store);
    c.context = r;
    c.ended = on_runtime_ended;
    return c;
}

typedef struct {
    hirsel_embed_config cfg;
    hirsel_status status;
    hirsel_runtime rt;
} start_arg;

static void *start_worker(void *p) {
    start_arg *a = (start_arg *)p;
    a->rt.generation = 777;
    a->status = hirsel_embed_v1_start(&a->cfg, &a->rt);
    return NULL;
}

typedef struct {
    hirsel_runtime rt;
    hirsel_status status;
} stop_arg;

static void *stop_worker(void *p) {
    stop_arg *a = (stop_arg *)p;
    a->status = hirsel_embed_v1_stop(a->rt);
    return NULL;
}

typedef struct {
    hirsel_session sess;
    _Atomic int stop_flag;
} call_hammer_arg;

static void *call_hammer_worker(void *p) {
    call_hammer_arg *a = (call_hammer_arg *)p;
    uint8_t payload[4] = { 9, 8, 7, 6 };
    uint64_t id = 100;
    while (!atomic_load_explicit(&a->stop_flag, memory_order_acquire)) {
        hirsel_status st = hirsel_embed_v1_call(a->sess, id++, 0, (hirsel_bytes){ payload, 4 }, 2000);
        if (st == HIRSEL_STOPPED) {
            break;
        }
    }
    return NULL;
}

int main(int argc, char **argv) {
    assert(argc == 2);
    const char *store = argv[1];
    embed_abi_fixture_init();
    assert(hirsel_embed_v1_abi() == HIRSEL_EMBED_ABI);

    uint8_t fp[32];
    embed_abi_fixture_manifest_fp(fp);

    /* M13: guard-page short struct & out == NULL */
    {
        runtime_ctx r = { .magic = 0xC0FFEE1234567890ULL };
        hirsel_embed_config cfg = make_cfg(fp, store, &r);
        assert(hirsel_embed_v1_start(&cfg, NULL) == HIRSEL_ABI_MISMATCH);

        long page = sysconf(_SC_PAGESIZE);
        uint8_t *pages = mmap(NULL, (size_t)page * 2, PROT_READ | PROT_WRITE,
                              MAP_PRIVATE | MAP_ANON, -1, 0);
        assert(pages != MAP_FAILED);
        assert(mprotect(pages + page, (size_t)page, PROT_NONE) == 0);
        uint8_t *short_cfg = pages + page - 8;
        uint32_t sz = 8;
        memcpy(short_cfg, &sz, 4);
        hirsel_runtime out = { 42 };
        assert(hirsel_embed_v1_start((const hirsel_embed_config *)short_cfg, &out) == HIRSEL_ABI_MISMATCH);
        assert(out.generation == 0);
        munmap(pages, (size_t)page * 2);
    }

    /* B2: 4 concurrent starts (1 OK, 3 ALREADY_RUNNING, *out == 0), start thread exits */
    embed_abi_fixture_set_mode(0);
    runtime_ctx *r1 = calloc(1, sizeof(*r1));
    r1->magic = 0xC0FFEE1234567890ULL;
    r1->free_on_end = 1;

    pthread_t th[4];
    start_arg sargs[4];
    for (int i = 0; i < 4; i++) {
        sargs[i].cfg = make_cfg(fp, store, r1);
        assert(pthread_create(&th[i], NULL, start_worker, &sargs[i]) == 0);
    }
    int ok_count = 0, already_count = 0;
    hirsel_runtime rt1 = { 0 };
    for (int i = 0; i < 4; i++) {
        pthread_join(th[i], NULL);
        if (sargs[i].status == HIRSEL_OK) {
            ok_count++;
            rt1 = sargs[i].rt;
        } else {
            assert(sargs[i].status == HIRSEL_ALREADY_RUNNING);
            assert(sargs[i].rt.generation == 0);
            already_count++;
        }
    }
    assert(ok_count == 1 && already_count == 3 && rt1.generation >= 1);

    /* B1 + ASan: host close waits for active callback, then session_ended frees context */
    {
        session_ctx *s = calloc(1, sizeof(*s));
        s->magic = 0x5E55104012345678ULL;
        s->hold_in_outcome = 1;
        s->free_on_end = 1;
        pthread_mutex_init(&s->mu, NULL);
        pthread_cond_init(&s->cv, NULL);
        hirsel_embed_callbacks cbs = {
            .size = (uint32_t)sizeof(hirsel_embed_callbacks),
            .context = s,
            .outcome = on_outcome,
            .change = on_change,
            .sub_ended = on_sub_ended,
            .room_event = on_room_event,
            .session_ended = on_session_ended,
        };
        hirsel_session sess = { 0, 0 };
        assert(hirsel_embed_v1_open(rt1, &cbs, (hirsel_bytes){ NULL, 0 }, &sess) == HIRSEL_OK);
        /* M13: overwrite stack callback table right after open */
        memset(&cbs, 0xDD, sizeof(cbs));

        uint8_t payload[2] = { 1, 2 };
        assert(hirsel_embed_v1_call(sess, 1, 0, (hirsel_bytes){ payload, 2 }, 5000) == HIRSEL_OK);

        pthread_mutex_lock(&s->mu);
        while (!s->in_outcome) {
            pthread_cond_wait(&s->cv, &s->mu);
        }
        s->release_outcome = 1;
        pthread_cond_broadcast(&s->cv);
        pthread_mutex_unlock(&s->mu);

        /* close blocks until outcome + session_ended return (which frees `s` under ASan!) */
        assert(hirsel_embed_v1_close(sess) == HIRSEL_OK);
        assert(hirsel_embed_v1_close(sess) == HIRSEL_STOPPED);
    }

    /* B1 + ASan: delivery-thread self-close -> DEFERRED, stop -> BUSY, losing host close -> STOPPED while session_ended hasn't run yet */
    {
        session_ctx *s = calloc(1, sizeof(*s));
        s->magic = 0x5E55104012345678ULL;
        s->rt = rt1;
        s->hold_in_outcome = 1;
        s->free_on_end = 0; /* wait on atomic, then free immediately after */
        pthread_mutex_init(&s->mu, NULL);
        pthread_cond_init(&s->cv, NULL);
        atomic_store_explicit(&s->self_close_mode, 1, memory_order_release);
        hirsel_embed_callbacks cbs = {
            .size = (uint32_t)sizeof(hirsel_embed_callbacks),
            .context = s,
            .outcome = on_outcome,
            .change = on_change,
            .sub_ended = on_sub_ended,
            .room_event = on_room_event,
            .session_ended = on_session_ended,
        };
        assert(hirsel_embed_v1_open(rt1, &cbs, (hirsel_bytes){ NULL, 0 }, &s->sess) == HIRSEL_OK);
        uint8_t payload[2] = { 3, 4 };
        assert(hirsel_embed_v1_call(s->sess, 2, 0, (hirsel_bytes){ payload, 2 }, 5000) == HIRSEL_OK);

        /* Wait until self-close ran (DEFERRED) and outcome is still held on delivery thread. */
        pthread_mutex_lock(&s->mu);
        while (!s->in_outcome) {
            pthread_cond_wait(&s->cv, &s->mu);
        }
        pthread_mutex_unlock(&s->mu);

        /* Losing host close returns HIRSEL_STOPPED immediately while session_ended has NOT run yet! */
        assert(hirsel_embed_v1_close(s->sess) == HIRSEL_STOPPED);
        assert(atomic_load_explicit(&s->session_ends, memory_order_acquire) == 0);

        /* Release delivery callback -> session_ended now runs while `s` is still live under ASan. */
        pthread_mutex_lock(&s->mu);
        s->release_outcome = 1;
        pthread_cond_broadcast(&s->cv);
        pthread_mutex_unlock(&s->mu);

        while (atomic_load_explicit(&s->session_ends, memory_order_acquire) == 0) {
            usleep(1000);
        }
        assert(atomic_load_explicit(&s->busy_rc, memory_order_acquire) == HIRSEL_BUSY);
        assert(atomic_load_explicit(&s->deferred_rc, memory_order_acquire) == HIRSEL_DEFERRED);
        assert(atomic_load_explicit(&s->losing_rc, memory_order_acquire) == HIRSEL_STOPPED);
        s->magic = 0xDEADBEEFDEADBEEFULL;
        pthread_mutex_destroy(&s->mu);
        pthread_cond_destroy(&s->cv);
        free(s);
    }

    /* B2: 2 concurrent stops -> 1 OK, 1 STOPPED; r1 freed inside on_runtime_ended under ASan */
    {
        pthread_t st[2];
        stop_arg sa[2] = { { rt1, -1 }, { rt1, -1 } };
        assert(pthread_create(&st[0], NULL, stop_worker, &sa[0]) == 0);
        assert(pthread_create(&st[1], NULL, stop_worker, &sa[1]) == 0);
        pthread_join(st[0], NULL);
        pthread_join(st[1], NULL);
        assert((sa[0].status == HIRSEL_OK && sa[1].status == HIRSEL_STOPPED) ||
               (sa[0].status == HIRSEL_STOPPED && sa[1].status == HIRSEL_OK));
    }

    /* B2 + ASan: spontaneous fault end frees runtime_ctx inside ended; stale rt1 rejected */
    {
        embed_abi_fixture_set_mode(1);
        runtime_ctx *r2 = calloc(1, sizeof(*r2));
        r2->magic = 0xC0FFEE1234567890ULL;
        r2->free_on_end = 0;
        hirsel_embed_config cfg = make_cfg(fp, store, r2);
        hirsel_runtime rt2 = { 0 };
        assert(hirsel_embed_v1_start(&cfg, &rt2) == HIRSEL_OK);
        assert(rt2.generation > rt1.generation);
        assert(hirsel_embed_v1_stop(rt1) == HIRSEL_STOPPED);
        while (atomic_load_explicit(&r2->ended_count, memory_order_acquire) == 0) {
            usleep(1000);
        }
        assert(atomic_load_explicit(&r2->end_code, memory_order_acquire) == HIRSEL_END_RUN_STOP);
        /* Free r2 right after ended returned; stop must NOT touch r2 under ASan! */
        r2->magic = 0xDEADBEEFDEADBEEFULL;
        free(r2);
        assert(hirsel_embed_v1_stop(rt2) == HIRSEL_OK);
    }

    /* B1/B2 + ASan: concurrent call producers racing `close` and `stop` with immediate free on end */
    {
        embed_abi_fixture_set_mode(0);
        runtime_ctx *r3 = calloc(1, sizeof(*r3));
        r3->magic = 0xC0FFEE1234567890ULL;
        r3->free_on_end = 1;
        hirsel_embed_config cfg = make_cfg(fp, store, r3);
        hirsel_runtime rt3 = { 0 };
        assert(hirsel_embed_v1_start(&cfg, &rt3) == HIRSEL_OK);

        session_ctx *s = calloc(1, sizeof(*s));
        s->magic = 0x5E55104012345678ULL;
        s->free_on_end = 1;
        pthread_mutex_init(&s->mu, NULL);
        pthread_cond_init(&s->cv, NULL);
        hirsel_embed_callbacks cbs = {
            .size = (uint32_t)sizeof(hirsel_embed_callbacks),
            .context = s,
            .outcome = on_outcome,
            .change = on_change,
            .sub_ended = on_sub_ended,
            .room_event = on_room_event,
            .session_ended = on_session_ended,
        };
        hirsel_session sess = { 0, 0 };
        assert(hirsel_embed_v1_open(rt3, &cbs, (hirsel_bytes){ NULL, 0 }, &sess) == HIRSEL_OK);
        call_hammer_arg ha = { .sess = sess, .stop_flag = 0 };
        pthread_t hw[2];
        assert(pthread_create(&hw[0], NULL, call_hammer_worker, &ha) == 0);
        assert(pthread_create(&hw[1], NULL, call_hammer_worker, &ha) == 0);
        usleep(10000);
        assert(hirsel_embed_v1_close(sess) == HIRSEL_OK);
        atomic_store_explicit(&ha.stop_flag, 1, memory_order_release);
        pthread_join(hw[0], NULL);
        pthread_join(hw[1], NULL);
        assert(hirsel_embed_v1_stop(rt3) == HIRSEL_OK);
    }

    printf("EMBED_ABI_ASAN_OK\n");
    return 0;
}
