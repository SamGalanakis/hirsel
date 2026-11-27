/* Real compiled Hirsel place host harness for Slice 2 laws.
 *
 * Wraps `hirsel_core_os_run_with_sources` (-Wl,--wrap=hirsel_core_os_run_with_sources)
 * so the real compiled Hirsel place program registers its OsConfig + entry thunk +
 * real native-Describe manifest JSON into the embed-slot `hirsel-core-rt` and then
 * executes every Slice 2 real-place scenario directly through `hirsel_embed_v1_*`.
 */
#define _GNU_SOURCE
#include "hirsel_core_rt_host.h"
#include "hirsel_core_rt_sites.h"
#include "hirsel_embed.h"

#include <assert.h>
#include <errno.h>
#include <pthread.h>
#include <signal.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/uio.h>
#include <sys/wait.h>
#include <unistd.h>

static _Atomic int g_fail_disk_writes = 0;

extern ssize_t __real_pwrite(int fd, const void *buf, size_t count, off_t offset);
extern ssize_t __real_pwritev(int fd, const struct iovec *iov, int iovcnt, off_t offset);

ssize_t __wrap_pwrite(int fd, const void *buf, size_t count, off_t offset) {
    if (fd > 2 && atomic_load_explicit(&g_fail_disk_writes, memory_order_acquire)) {
        errno = ENOSPC;
        return -1;
    }
    return __real_pwrite(fd, buf, count, offset);
}

ssize_t __wrap_pwritev(int fd, const struct iovec *iov, int iovcnt, off_t offset) {
    if (fd > 2 && atomic_load_explicit(&g_fail_disk_writes, memory_order_acquire)) {
        errno = ENOSPC;
        return -1;
    }
    return __real_pwritev(fd, iov, iovcnt, offset);
}

extern int32_t hirsel_embed_harness_register(
    const HirselCoreOsConfig *input,
    const HirselCoreView *names,
    const HirselCoreSourceTable *sources,
    HirselCoreEntry entry,
    const uint8_t *manifest_ptr,
    size_t manifest_len,
    uint8_t foreign_waits);

extern int32_t hirsel_embed_harness_manifest_fp(
    const uint8_t *manifest_ptr, size_t manifest_len, uint8_t out_32[32]);

extern intptr_t hirsel_embed_harness_welcome_target(
    uint64_t gen_id, uint8_t *out, size_t cap);

#define PROGRESS_FLOOR (262144ULL + 14ULL + 8192ULL)

/* Entry indices in `embed_place.hirsel` registration order:
 * 0: put(PutReq{key:u64, val:Bytes}) -> Result[u64, u64]
 * 1: get(u64) -> Option[Bytes]
 * 2: list_items(()) -> () [slot 0: items]
 * 3: chat(()) room
 * 4: write_dirs(Bytes) -> u64
 * 5: read_data(()) -> Bytes
 */
#define ENTRY_PUT 0
#define ENTRY_GET 1
#define ENTRY_LIST 2
#define ENTRY_CHAT 3
#define ENTRY_WRITE_DIRS 4
#define ENTRY_READ_DATA 5

typedef struct {
    pthread_mutex_t mu;
    pthread_cond_t cv;
    int ended;
    uint32_t code;
    char reason[512];
} rt_rec;

typedef struct {
    pthread_mutex_t mu;
    pthread_cond_t cv;
    uint64_t last_call;
    int outcome_count;
    uint8_t last_outcome[65536];
    size_t last_outcome_len;
    int sub_end_count;
    uint32_t last_sub_end_tag;
    int change_count;
    int session_ended;
    int hold_change;
    int in_change;
    int release_change;
} sess_rec;

static void rt_rec_init(rt_rec *r) {
    memset(r, 0, sizeof(*r));
    pthread_mutex_init(&r->mu, NULL);
    pthread_cond_init(&r->cv, NULL);
}

static void sess_rec_init(sess_rec *s) {
    memset(s, 0, sizeof(*s));
    pthread_mutex_init(&s->mu, NULL);
    pthread_cond_init(&s->cv, NULL);
}

static void on_rt_ended(void *ctx, uint32_t code, hirsel_bytes reason) {
    rt_rec *r = (rt_rec *)ctx;
    if (!r) return;
    pthread_mutex_lock(&r->mu);
    r->ended++;
    r->code = code;
    size_t n = reason.len < sizeof(r->reason) - 1 ? reason.len : sizeof(r->reason) - 1;
    if (n > 0) memcpy(r->reason, reason.ptr, n);
    r->reason[n] = '\0';
    pthread_cond_broadcast(&r->cv);
    pthread_mutex_unlock(&r->mu);
}

static void on_outcome(void *ctx, uint64_t call, hirsel_bytes out) {
    sess_rec *s = (sess_rec *)ctx;
    pthread_mutex_lock(&s->mu);
    s->last_call = call;
    s->last_outcome_len = out.len < sizeof(s->last_outcome) ? out.len : sizeof(s->last_outcome);
    if (s->last_outcome_len > 0) memcpy(s->last_outcome, out.ptr, s->last_outcome_len);
    s->outcome_count++;
    pthread_cond_broadcast(&s->cv);
    pthread_mutex_unlock(&s->mu);
}

static void on_change(void *ctx, uint64_t sub, hirsel_bytes ch) {
    (void)sub; (void)ch;
    sess_rec *s = (sess_rec *)ctx;
    pthread_mutex_lock(&s->mu);
    s->change_count++;
    if (s->hold_change) {
        s->in_change = 1;
        pthread_cond_broadcast(&s->cv);
        while (!s->release_change) {
            pthread_cond_wait(&s->cv, &s->mu);
        }
    }
    pthread_cond_broadcast(&s->cv);
    pthread_mutex_unlock(&s->mu);
}

static void on_sub_ended(void *ctx, uint64_t sub, hirsel_bytes end) {
    (void)sub;
    sess_rec *s = (sess_rec *)ctx;
    pthread_mutex_lock(&s->mu);
    if (end.len >= 4) {
        uint32_t tag = 0;
        memcpy(&tag, end.ptr, 4);
        s->last_sub_end_tag = tag;
    }
    s->sub_end_count++;
    pthread_cond_broadcast(&s->cv);
    pthread_mutex_unlock(&s->mu);
}

static void on_room_event(void *ctx, uint64_t room, hirsel_bytes ev) {
    (void)ctx; (void)room; (void)ev;
}

static void on_session_ended(void *ctx, hirsel_bytes end) {
    (void)end;
    sess_rec *s = (sess_rec *)ctx;
    pthread_mutex_lock(&s->mu);
    s->session_ended++;
    pthread_cond_broadcast(&s->cv);
    pthread_mutex_unlock(&s->mu);
}

static hirsel_embed_callbacks make_cbs(sess_rec *s) {
    return (hirsel_embed_callbacks){
        .size = (uint32_t)sizeof(hirsel_embed_callbacks),
        .context = s,
        .outcome = on_outcome,
        .change = on_change,
        .sub_ended = on_sub_ended,
        .room_event = on_room_event,
        .session_ended = on_session_ended,
    };
}

static const uint8_t CRED16[16] = {
    1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16
};

static void wait_outcomes(sess_rec *s, int expected) {
    pthread_mutex_lock(&s->mu);
    while (s->outcome_count < expected && !s->session_ended) {
        pthread_cond_wait(&s->cv, &s->mu);
    }
    int ended = s->session_ended;
    int got = s->outcome_count;
    pthread_mutex_unlock(&s->mu);
    if (got < expected) {
        fprintf(stderr, "session ended prematurely while waiting for %d outcomes (got %d, ended=%d)\n", expected, got, ended);
        assert(got >= expected);
    }
}

static void wait_sub_ended(sess_rec *s, int expected) {
    pthread_mutex_lock(&s->mu);
    while (s->sub_end_count < expected && !s->session_ended) {
        pthread_cond_wait(&s->cv, &s->mu);
    }
    int ended = s->session_ended;
    int got = s->sub_end_count;
    pthread_mutex_unlock(&s->mu);
    if (got < expected) {
        fprintf(stderr, "session ended prematurely while waiting for %d sub_ends (got %d, ended=%d)\n", expected, got, ended);
        assert(got >= expected);
    }
}

static void wait_rt_ended(rt_rec *r) {
    pthread_mutex_lock(&r->mu);
    while (!r->ended) {
        pthread_cond_wait(&r->cv, &r->mu);
    }
    pthread_mutex_unlock(&r->mu);
}

/* Wire codecs matching Hirsel std.codec:
 * u64 = 8B LE, Bytes = u32 len LE + bytes.
 */
static size_t enc_put_req(uint8_t *buf, uint64_t key, const uint8_t *val, uint32_t vlen) {
    memcpy(buf, &key, 8);
    memcpy(buf + 8, &vlen, 4);
    if (vlen > 0) memcpy(buf + 12, val, vlen);
    return 12 + vlen;
}

static size_t enc_bytes(uint8_t *buf, const uint8_t *val, uint32_t vlen) {
    memcpy(buf, &vlen, 4);
    if (vlen > 0) memcpy(buf + 4, val, vlen);
    return 4 + vlen;
}

static void print_hex(const char *label, const uint8_t *buf, size_t len) {
    printf("%s=", label);
    for (size_t i = 0; i < len; i++) {
        printf("%02x", buf[i]);
    }
    printf("\n");
}

static hirsel_embed_config make_cfg(
    const uint8_t fp[32],
    const char *store_dir,
    const char *grants,
    const char *dir_bases,
    uint64_t n_place,
    uint64_t client_bytes,
    rt_rec *rec)
{
    hirsel_embed_config c;
    memset(&c, 0, sizeof(c));
    c.size = (uint32_t)sizeof(c);
    c.abi = HIRSEL_EMBED_ABI;
    c.manifest_fp.ptr = fp;
    c.manifest_fp.len = 32;
    c.n_place = n_place;
    c.client_bytes = client_bytes;
    c.start_timeout_ms = 10000;
    c.grants.ptr = (const uint8_t *)grants;
    c.grants.len = strlen(grants);
    c.dir_bases.ptr = (const uint8_t *)dir_bases;
    c.dir_bases.len = strlen(dir_bases);
    c.store_dir.ptr = (const uint8_t *)store_dir;
    c.store_dir.len = strlen(store_dir);
    c.context = rec;
    c.ended = on_rt_ended;
    return c;
}

static uint8_t *read_file_all(const char *path, size_t *out_len) {
    FILE *f = fopen(path, "rb");
    assert(f != NULL);
    fseek(f, 0, SEEK_END);
    long sz = ftell(f);
    assert(sz > 0);
    fseek(f, 0, SEEK_SET);
    uint8_t *buf = malloc((size_t)sz);
    assert(buf != NULL);
    assert(fread(buf, 1, (size_t)sz, f) == (size_t)sz);
    fclose(f);
    *out_len = (size_t)sz;
    return buf;
}

uint8_t __wrap_hirsel_core_os_run_with_sources(
    const HirselCoreOsConfig *input,
    const HirselCoreView *names,
    const HirselCoreSourceTable *sources,
    HirselCoreEntry entry)
{
    const char *manifest_path = getenv("HIRSEL_EMBED_MANIFEST");
    const char *mode = getenv("HIRSEL_EMBED_MODE");
    const char *store_dir = getenv("HIRSEL_EMBED_STORE");
    const char *grants = getenv("HIRSEL_EMBED_GRANTS");
    const char *dir_bases = getenv("HIRSEL_EMBED_DIR_BASES");
    assert(manifest_path && mode && store_dir && grants && dir_bases);

    size_t mlen = 0;
    uint8_t *mbytes = read_file_all(manifest_path, &mlen);
    assert(hirsel_embed_harness_register(input, names, sources, entry, mbytes, mlen, 0) == 0);
    uint8_t fp[32];
    assert(hirsel_embed_harness_manifest_fp(mbytes, mlen, fp) == 0);

    if (strcmp(mode, "m2-and-m2-target") == 0) {
        /* 1. Start, commit two rows, check Welcome.target (m2), immediate stop -> immediate start -> query! */
        rt_rec r1; rt_rec_init(&r1);
        hirsel_embed_config cfg = make_cfg(fp, store_dir, grants, dir_bases, 16ULL << 20, PROGRESS_FLOOR + (1ULL << 20), &r1);
        hirsel_runtime rt = { 0 };
        hirsel_status st = hirsel_embed_v1_start(&cfg, &rt);
        if (st != HIRSEL_OK) {
            fprintf(stderr, "start1 failed st=%d code=%u reason=%s\n", st, r1.code, r1.reason);
            return 1;
        }
        sess_rec s1; sess_rec_init(&s1);
        hirsel_embed_callbacks cbs = make_cbs(&s1);
        hirsel_session sess = { 0, 0 };
        assert(hirsel_embed_v1_open(rt, &cbs, (hirsel_bytes){ CRED16, 16 }, &sess) == HIRSEL_OK);

        uint8_t req[64];
        size_t n = enc_put_req(req, 1, (const uint8_t *)"alpha", 5);
        assert(hirsel_embed_v1_call(sess, 1, ENTRY_PUT, (hirsel_bytes){ req, n }, 5000) == HIRSEL_OK);
        wait_outcomes(&s1, 1);
        print_hex("put1", s1.last_outcome, s1.last_outcome_len);

        /* Verify m2: Welcome.target equals compiled target triple! */
        uint8_t target_buf[128];
        intptr_t tlen = hirsel_embed_harness_welcome_target(rt.generation, target_buf, sizeof(target_buf) - 1);
        assert(tlen > 0);
        target_buf[tlen] = '\0';
        printf("welcome_target=%s\n", (char *)target_buf);

        n = enc_put_req(req, 2, (const uint8_t *)"beta", 4);
        assert(hirsel_embed_v1_call(sess, 2, ENTRY_PUT, (hirsel_bytes){ req, n }, 5000) == HIRSEL_OK);
        wait_outcomes(&s1, 2);
        print_hex("put2", s1.last_outcome, s1.last_outcome_len);

        assert(hirsel_embed_v1_close(sess) == HIRSEL_OK);
        assert(hirsel_embed_v1_stop(rt) == HIRSEL_OK);

        /* Immediate restart on same store_dir: zero writer-lock wait! */
        rt_rec r2; rt_rec_init(&r2);
        hirsel_embed_config cfg2 = make_cfg(fp, store_dir, grants, dir_bases, 16ULL << 20, PROGRESS_FLOOR + (1ULL << 20), &r2);
        hirsel_runtime rt2 = { 0 };
        st = hirsel_embed_v1_start(&cfg2, &rt2);
        if (st != HIRSEL_OK) {
            fprintf(stderr, "immediate restart failed st=%d reason=%s\n", st, r2.reason);
            return 2;
        }
        sess_rec s2; sess_rec_init(&s2);
        hirsel_embed_callbacks cbs2 = make_cbs(&s2);
        hirsel_session sess2 = { 0, 0 };
        assert(hirsel_embed_v1_open(rt2, &cbs2, (hirsel_bytes){ CRED16, 16 }, &sess2) == HIRSEL_OK);

        uint64_t k1 = 1, k2 = 2, k999 = 999;
        assert(hirsel_embed_v1_call(sess2, 10, ENTRY_GET, (hirsel_bytes){ (const uint8_t *)&k1, 8 }, 5000) == HIRSEL_OK);
        wait_outcomes(&s2, 1);
        print_hex("get1", s2.last_outcome, s2.last_outcome_len);

        assert(hirsel_embed_v1_call(sess2, 11, ENTRY_GET, (hirsel_bytes){ (const uint8_t *)&k2, 8 }, 5000) == HIRSEL_OK);
        wait_outcomes(&s2, 2);
        print_hex("get2", s2.last_outcome, s2.last_outcome_len);

        assert(hirsel_embed_v1_call(sess2, 12, ENTRY_GET, (hirsel_bytes){ (const uint8_t *)&k999, 8 }, 5000) == HIRSEL_OK);
        wait_outcomes(&s2, 3);
        print_hex("get999", s2.last_outcome, s2.last_outcome_len);

        assert(hirsel_embed_v1_close(sess2) == HIRSEL_OK);
        assert(hirsel_embed_v1_stop(rt2) == HIRSEL_OK);
        printf("OK\n");
        return 0;
    }

    if (strcmp(mode, "m2-crash-and-reopen") == 0) {
        /* Fork child: commits key 10 ("before-kill"), starts key 11, gets SIGKILLed without stop! */
        int pipefd[2];
        assert(pipe(pipefd) == 0);
        pid_t pid = fork();
        assert(pid >= 0);
        if (pid == 0) {
            close(pipefd[0]);
            rt_rec rc; rt_rec_init(&rc);
            hirsel_embed_config cfg = make_cfg(fp, store_dir, grants, dir_bases, 16ULL << 20, PROGRESS_FLOOR + (1ULL << 20), &rc);
            hirsel_runtime rt = { 0 };
            assert(hirsel_embed_v1_start(&cfg, &rt) == HIRSEL_OK);
            sess_rec sc; sess_rec_init(&sc);
            hirsel_embed_callbacks cbs = make_cbs(&sc);
            hirsel_session sess = { 0, 0 };
            assert(hirsel_embed_v1_open(rt, &cbs, (hirsel_bytes){ CRED16, 16 }, &sess) == HIRSEL_OK);

            uint8_t req[64];
            size_t n = enc_put_req(req, 10, (const uint8_t *)"before-kill", 11);
            assert(hirsel_embed_v1_call(sess, 1, ENTRY_PUT, (hirsel_bytes){ req, n }, 5000) == HIRSEL_OK);
            wait_outcomes(&sc, 1);

            /* Notify parent that key 10 is acknowledged, then fire key 11 and pause to be SIGKILLed! */
            char b = 'K';
            assert(write(pipefd[1], &b, 1) == 1);
            n = enc_put_req(req, 11, (const uint8_t *)"maybe-after", 11);
            (void)hirsel_embed_v1_call(sess, 2, ENTRY_PUT, (hirsel_bytes){ req, n }, 5000);
            for (;;) pause();
        }
        close(pipefd[1]);
        char ack = 0;
        assert(read(pipefd[0], &ack, 1) == 1 && ack == 'K');
        close(pipefd[0]);
        assert(kill(pid, SIGKILL) == 0);
        int status = 0;
        assert(waitpid(pid, &status, 0) == pid);
        assert(WIFSIGNALED(status) && WTERMSIG(status) == SIGKILL);

        /* Reopen store after SIGKILL: key 10 MUST survive, key 11 must be absent or wholly "maybe-after". */
        rt_rec r2; rt_rec_init(&r2);
        hirsel_embed_config cfg2 = make_cfg(fp, store_dir, grants, dir_bases, 16ULL << 20, PROGRESS_FLOOR + (1ULL << 20), &r2);
        hirsel_runtime rt2 = { 0 };
        hirsel_status st = hirsel_embed_v1_start(&cfg2, &rt2);
        if (st != HIRSEL_OK) {
            fprintf(stderr, "reopen after SIGKILL failed st=%d reason=%s\n", st, r2.reason);
            return 3;
        }
        sess_rec s2; sess_rec_init(&s2);
        hirsel_embed_callbacks cbs2 = make_cbs(&s2);
        hirsel_session sess2 = { 0, 0 };
        assert(hirsel_embed_v1_open(rt2, &cbs2, (hirsel_bytes){ CRED16, 16 }, &sess2) == HIRSEL_OK);
        uint64_t k10 = 10, k11 = 11;
        assert(hirsel_embed_v1_call(sess2, 1, ENTRY_GET, (hirsel_bytes){ (const uint8_t *)&k10, 8 }, 5000) == HIRSEL_OK);
        wait_outcomes(&s2, 1);
        print_hex("crash_get10", s2.last_outcome, s2.last_outcome_len);
        assert(hirsel_embed_v1_call(sess2, 2, ENTRY_GET, (hirsel_bytes){ (const uint8_t *)&k11, 8 }, 5000) == HIRSEL_OK);
        wait_outcomes(&s2, 2);
        print_hex("crash_get11", s2.last_outcome, s2.last_outcome_len);
        assert(hirsel_embed_v1_close(sess2) == HIRSEL_OK);
        assert(hirsel_embed_v1_stop(rt2) == HIRSEL_OK);
        printf("OK\n");
        return 0;
    }

    if (strcmp(mode, "m2-corrupt-refused") == 0) {
        rt_rec r; rt_rec_init(&r);
        hirsel_embed_config cfg = make_cfg(fp, store_dir, grants, dir_bases, 16ULL << 20, PROGRESS_FLOOR + (1ULL << 20), &r);
        hirsel_runtime rt = { 77 };
        hirsel_status st = hirsel_embed_v1_start(&cfg, &rt);
        printf("corrupt_status=%d gen=%llu code=%u reason=%s\n", st, (unsigned long long)rt.generation, r.code, r.reason);
        assert(st == HIRSEL_REFUSED);
        assert(rt.generation == 0);
        assert(r.ended == 1 && r.code == HIRSEL_END_REFUSED);
        printf("OK\n");
        return 0;
    }

    if (strcmp(mode, "m11-real-capacity") == 0) {
        /* 1. Too small n_place -> HIRSEL_REFUSED + sync ended with PlaceCapacity(Room), *out == 0 */
        rt_rec r1; rt_rec_init(&r1);
        hirsel_embed_config cfg_small = make_cfg(fp, store_dir, grants, dir_bases, 1024ULL, PROGRESS_FLOOR + (1ULL << 20), &r1);
        hirsel_runtime rt1 = { 55 };
        hirsel_status st = hirsel_embed_v1_start(&cfg_small, &rt1);
        fprintf(stderr, "small_status=%d gen=%llu code=%u reason=%s\n", st, (unsigned long long)rt1.generation, r1.code, r1.reason);
        fflush(stderr);
        assert(st == HIRSEL_REFUSED);
        assert(rt1.generation == 0);
        assert(r1.ended == 1 && r1.code == HIRSEL_END_REFUSED);
        assert(strstr(r1.reason, "PlaceCapacity") != NULL);

        /* 2. Subsequent start at fitting n_place succeeds cleanly! */
        rt_rec r2; rt_rec_init(&r2);
        hirsel_embed_config cfg_ok = make_cfg(fp, store_dir, grants, dir_bases, 16ULL << 20, PROGRESS_FLOOR + (1ULL << 20), &r2);
        hirsel_runtime rt2 = { 0 };
        assert(hirsel_embed_v1_start(&cfg_ok, &rt2) == HIRSEL_OK);
        assert(rt2.generation > 0);
        assert(hirsel_embed_v1_stop(rt2) == HIRSEL_OK);
        printf("OK\n");
        return 0;
    }

    if (strcmp(mode, "m4-dirs-write") == 0) {
        rt_rec r; rt_rec_init(&r);
        hirsel_embed_config cfg = make_cfg(fp, store_dir, grants, dir_bases, 16ULL << 20, PROGRESS_FLOOR + (1ULL << 20), &r);
        hirsel_runtime rt = { 0 };
        hirsel_status st = hirsel_embed_v1_start(&cfg, &rt);
        if (st != HIRSEL_OK) {
            fprintf(stderr, "m4-dirs-write start failed st=%d reason=%s\n", st, r.reason);
            return 4;
        }
        sess_rec s; sess_rec_init(&s);
        hirsel_embed_callbacks cbs = make_cbs(&s);
        hirsel_session sess = { 0, 0 };
        assert(hirsel_embed_v1_open(rt, &cbs, (hirsel_bytes){ CRED16, 16 }, &sess) == HIRSEL_OK);
        uint8_t req[64];
        size_t n = enc_bytes(req, (const uint8_t *)"persisted-v1", 12);
        assert(hirsel_embed_v1_call(sess, 1, ENTRY_WRITE_DIRS, (hirsel_bytes){ req, n }, 5000) == HIRSEL_OK);
        wait_outcomes(&s, 1);
        print_hex("write_dirs", s.last_outcome, s.last_outcome_len);
        assert(hirsel_embed_v1_close(sess) == HIRSEL_OK);
        assert(hirsel_embed_v1_stop(rt) == HIRSEL_OK);
        printf("OK\n");
        return 0;
    }

    if (strcmp(mode, "m4-dirs-read") == 0) {
        rt_rec r; rt_rec_init(&r);
        hirsel_embed_config cfg = make_cfg(fp, store_dir, grants, dir_bases, 16ULL << 20, PROGRESS_FLOOR + (1ULL << 20), &r);
        hirsel_runtime rt = { 0 };
        assert(hirsel_embed_v1_start(&cfg, &rt) == HIRSEL_OK);
        sess_rec s; sess_rec_init(&s);
        hirsel_embed_callbacks cbs = make_cbs(&s);
        hirsel_session sess = { 0, 0 };
        assert(hirsel_embed_v1_open(rt, &cbs, (hirsel_bytes){ CRED16, 16 }, &sess) == HIRSEL_OK);
        assert(hirsel_embed_v1_call(sess, 1, ENTRY_READ_DATA, (hirsel_bytes){ NULL, 0 }, 5000) == HIRSEL_OK);
        wait_outcomes(&s, 1);
        print_hex("read_data", s.last_outcome, s.last_outcome_len);
        assert(hirsel_embed_v1_close(sess) == HIRSEL_OK);
        assert(hirsel_embed_v1_stop(rt) == HIRSEL_OK);
        printf("OK\n");
        return 0;
    }

    if (strcmp(mode, "m4-refused") == 0) {
        rt_rec r; rt_rec_init(&r);
        hirsel_embed_config cfg = make_cfg(fp, store_dir, grants, dir_bases, 16ULL << 20, PROGRESS_FLOOR + (1ULL << 20), &r);
        hirsel_runtime rt = { 99 };
        hirsel_status st = hirsel_embed_v1_start(&cfg, &rt);
        printf("refused_status=%d gen=%llu reason=%s\n", st, (unsigned long long)rt.generation, r.reason);
        assert(st == HIRSEL_REFUSED && rt.generation == 0);
        printf("OK\n");
        return 0;
    }

    if (strcmp(mode, "m3-sub-client-capacity") == 0) {
        /* 1. Seed large rows with generous client_bytes. */
        rt_rec r1; rt_rec_init(&r1);
        hirsel_embed_config cfg1 = make_cfg(fp, store_dir, grants, dir_bases, 16ULL << 20, PROGRESS_FLOOR + (1ULL << 20), &r1);
        hirsel_runtime rt1 = { 0 };
        assert(hirsel_embed_v1_start(&cfg1, &rt1) == HIRSEL_OK);
        sess_rec s1; sess_rec_init(&s1);
        hirsel_embed_callbacks cbs1 = make_cbs(&s1);
        hirsel_session sess1 = { 0, 0 };
        assert(hirsel_embed_v1_open(rt1, &cbs1, (hirsel_bytes){ CRED16, 16 }, &sess1) == HIRSEL_OK);
        uint8_t big_val[4096];
        memset(big_val, 0x5A, sizeof(big_val));
        uint8_t req[4200];
        for (uint64_t k = 1; k <= 6; k++) {
            size_t n = enc_put_req(req, k, big_val, sizeof(big_val));
            assert(hirsel_embed_v1_call(sess1, k, ENTRY_PUT, (hirsel_bytes){ req, n }, 5000) == HIRSEL_OK);
        }
        wait_outcomes(&s1, 6);
        assert(s1.last_outcome_len >= 4 && s1.last_outcome[0] == 0);
        assert(hirsel_embed_v1_close(sess1) == HIRSEL_OK);
        assert(hirsel_embed_v1_stop(rt1) == HIRSEL_OK);

        /* 2. Reopen with tight client_bytes (just enough for session + sub base, NOT 24 KiB image!). */
        rt_rec r2; rt_rec_init(&r2);
        hirsel_embed_config cfg2 = make_cfg(fp, store_dir, grants, dir_bases, 16ULL << 20, PROGRESS_FLOOR + 8192ULL, &r2);
        hirsel_runtime rt2 = { 0 };
        assert(hirsel_embed_v1_start(&cfg2, &rt2) == HIRSEL_OK);
        sess_rec s2; sess_rec_init(&s2);
        hirsel_embed_callbacks cbs2 = make_cbs(&s2);
        hirsel_session sess2 = { 0, 0 };
        assert(hirsel_embed_v1_open(rt2, &cbs2, (hirsel_bytes){ CRED16, 16 }, &sess2) == HIRSEL_OK);
        assert(hirsel_embed_v1_subscribe(sess2, 1, ENTRY_LIST, (hirsel_bytes){ NULL, 0 }) == HIRSEL_OK);
        wait_sub_ended(&s2, 1);
        printf("sub_end_tag=%u\n", s2.last_sub_end_tag);
        assert(s2.last_sub_end_tag == 1); /* SubEnd::ClientCapacity = tag 1 */
        assert(hirsel_embed_v1_close(sess2) == HIRSEL_OK);
        assert(hirsel_embed_v1_stop(rt2) == HIRSEL_OK);
        printf("OK\n");
        return 0;
    }

    if (strcmp(mode, "m2-transient-poison") == 0) {
        /* Commit key 20 ("durable"), then inject transient ENOSPC via __wrap_pwrite/pwritev
         * so the open store Db poisons and ends with HIRSEL_END_PLACE_FAILED; clear the fault,
         * stop, restart on the same store_dir, and verify start succeeds & key 20 is intact! */
        rt_rec r1; rt_rec_init(&r1);
        hirsel_embed_config cfg1 = make_cfg(fp, store_dir, grants, dir_bases, 16ULL << 20, PROGRESS_FLOOR + (1ULL << 20), &r1);
        hirsel_runtime rt1 = { 0 };
        assert(hirsel_embed_v1_start(&cfg1, &rt1) == HIRSEL_OK);
        sess_rec s1; sess_rec_init(&s1);
        hirsel_embed_callbacks cbs1 = make_cbs(&s1);
        hirsel_session sess1 = { 0, 0 };
        assert(hirsel_embed_v1_open(rt1, &cbs1, (hirsel_bytes){ CRED16, 16 }, &sess1) == HIRSEL_OK);

        uint8_t req[64];
        size_t n = enc_put_req(req, 20, (const uint8_t *)"durable", 7);
        assert(hirsel_embed_v1_call(sess1, 1, ENTRY_PUT, (hirsel_bytes){ req, n }, 5000) == HIRSEL_OK);
        wait_outcomes(&s1, 1);

        /* Inject transient ENOSPC on disk writes and issue a second commit to poison Db! */
        atomic_store_explicit(&g_fail_disk_writes, 1, memory_order_release);
        n = enc_put_req(req, 21, (const uint8_t *)"poison-trigger", 14);
        (void)hirsel_embed_v1_call(sess1, 2, ENTRY_PUT, (hirsel_bytes){ req, n }, 5000);
        wait_rt_ended(&r1);
        atomic_store_explicit(&g_fail_disk_writes, 0, memory_order_release);
        printf("poison_code=%u poison_reason=%s\n", r1.code, r1.reason);
        assert(r1.code == HIRSEL_END_PLACE_FAILED);
        assert(hirsel_embed_v1_stop(rt1) == HIRSEL_OK);

        /* Reopen cleanly on same store_dir: transient poison MUST NOT persist, key 20 intact! */
        rt_rec r2; rt_rec_init(&r2);
        hirsel_embed_config cfg2 = make_cfg(fp, store_dir, grants, dir_bases, 16ULL << 20, PROGRESS_FLOOR + (1ULL << 20), &r2);
        hirsel_runtime rt2 = { 0 };
        hirsel_status st2 = hirsel_embed_v1_start(&cfg2, &rt2);
        if (st2 != HIRSEL_OK) {
            fprintf(stderr, "restart after transient poison failed st=%d reason=%s\n", st2, r2.reason);
            return 5;
        }
        sess_rec s2; sess_rec_init(&s2);
        hirsel_embed_callbacks cbs2 = make_cbs(&s2);
        hirsel_session sess2 = { 0, 0 };
        assert(hirsel_embed_v1_open(rt2, &cbs2, (hirsel_bytes){ CRED16, 16 }, &sess2) == HIRSEL_OK);
        uint64_t k20 = 20;
        assert(hirsel_embed_v1_call(sess2, 1, ENTRY_GET, (hirsel_bytes){ (const uint8_t *)&k20, 8 }, 5000) == HIRSEL_OK);
        wait_outcomes(&s2, 1);
        print_hex("poison_get20", s2.last_outcome, s2.last_outcome_len);
        assert(hirsel_embed_v1_close(sess2) == HIRSEL_OK);
        assert(hirsel_embed_v1_stop(rt2) == HIRSEL_OK);
        printf("OK\n");
        return 0;
    }

    if (strcmp(mode, "diff-embed") == 0) {
        rt_rec r; rt_rec_init(&r);
        hirsel_embed_config cfg = make_cfg(fp, store_dir, grants, dir_bases, 16ULL << 20, PROGRESS_FLOOR + (1ULL << 20), &r);
        hirsel_runtime rt = { 0 };
        assert(hirsel_embed_v1_start(&cfg, &rt) == HIRSEL_OK);
        sess_rec s; sess_rec_init(&s);
        hirsel_embed_callbacks cbs = make_cbs(&s);
        hirsel_session sess = { 0, 0 };
        assert(hirsel_embed_v1_open(rt, &cbs, (hirsel_bytes){ CRED16, 16 }, &sess) == HIRSEL_OK);

        uint8_t req[64];
        size_t n = enc_put_req(req, 100, (const uint8_t *)"diff-hello", 10);
        assert(hirsel_embed_v1_call(sess, 1, ENTRY_PUT, (hirsel_bytes){ req, n }, 5000) == HIRSEL_OK);
        wait_outcomes(&s, 1);
        print_hex("diff_put100", s.last_outcome, s.last_outcome_len);

        uint64_t k100 = 100, k999 = 999;
        assert(hirsel_embed_v1_call(sess, 2, ENTRY_GET, (hirsel_bytes){ (const uint8_t *)&k100, 8 }, 5000) == HIRSEL_OK);
        wait_outcomes(&s, 2);
        print_hex("diff_get100", s.last_outcome, s.last_outcome_len);

        assert(hirsel_embed_v1_call(sess, 3, ENTRY_GET, (hirsel_bytes){ (const uint8_t *)&k999, 8 }, 5000) == HIRSEL_OK);
        wait_outcomes(&s, 3);
        print_hex("diff_get999", s.last_outcome, s.last_outcome_len);

        assert(hirsel_embed_v1_close(sess) == HIRSEL_OK);
        assert(hirsel_embed_v1_stop(rt) == HIRSEL_OK);
        printf("OK\n");
        return 0;
    }

    (void)wait_rt_ended;
    fprintf(stderr, "unknown HIRSEL_EMBED_MODE=%s\n", mode);
    return 99;
}
