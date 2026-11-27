#define _POSIX_C_SOURCE 200809L
#include "hirsel_product.h"

#include <assert.h>
#include <pthread.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/wait.h>
#include <unistd.h>

static pthread_mutex_t lock = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t delivered = PTHREAD_COND_INITIALIZER;
static size_t length;
static uint8_t answer[64];
static unsigned ended;
static uint32_t end_code;
static char end_reason[128];
static pid_t host_pid;

static hirsel_bytes text(const char *str) {
    return (hirsel_bytes){(const uint8_t *)str, strlen(str)};
}

_Noreturn void __real_exit(int status);
_Noreturn void __real__exit(int status);

_Noreturn void __wrap_exit(int status) {
    if (host_pid && getpid() != host_pid) __real_exit(status);
    fprintf(stderr, "archive called exit(%d)\n", status);
    abort();
}

_Noreturn void __wrap__exit(int status) {
    if (host_pid && getpid() != host_pid) __real__exit(status);
    fprintf(stderr, "archive called _exit(%d)\n", status);
    abort();
}

static void runtime_ended(void *context, uint32_t code, hirsel_bytes reason) {
    (void)context;
    assert(reason.len < sizeof end_reason);
    if (reason.len) memcpy(end_reason, reason.ptr, reason.len);
    end_reason[reason.len] = '\0';
    end_code = code;
    ++ended;
}

static void outcome(void *context, uint64_t id, hirsel_bytes bytes) {
    (void)context;
    assert(id == 1 && bytes.len && bytes.len <= sizeof answer);
    pthread_mutex_lock(&lock);
    memcpy(answer, bytes.ptr, bytes.len);
    length = bytes.len;
    pthread_cond_signal(&delivered);
    pthread_mutex_unlock(&lock);
}

static void unused_event(void *context, uint64_t id, hirsel_bytes bytes) {
    (void)context;
    (void)id;
    (void)bytes;
    abort();
}

static void session_ended(void *context, hirsel_bytes end) {
    (void)context;
    (void)end;
}

extern uint64_t adjacent_value(void);

static void check_child_exit(void (*terminate)(int)) {
    pid_t child = fork();
    assert(child >= 0);
    if (child == 0) {
        terminate(23);
        abort();
    }
    int status;
    assert(waitpid(child, &status, 0) == child);
    assert(WIFEXITED(status) && WEXITSTATUS(status) == 23);
}

int main(int argc, char **argv) {
    host_pid = getpid();
    REQUIRE_ANCHOR();
    assert(hirsel_embed_v1_abi() == HIRSEL_EMBED_ABI);
    assert(adjacent_value() == 71);
    if (argc == 1) {
        hirsel_bytes manifest = hirsel_embed_v1_manifest();
        assert(manifest.len > 0);
        assert(fwrite(manifest.ptr, 1, manifest.len, stdout) == manifest.len);
        return 0;
    }
    assert(argc == 6);
    if (!strcmp(argv[4], "call")) {
        check_child_exit(exit);
        check_child_exit(_exit);
    }
    assert(signal(SIGPIPE, SIG_DFL) != SIG_ERR);
    uint8_t fingerprint[32];
    FILE *fp = fopen(argv[1], "rb");
    assert(fp && fread(fingerprint, 1, sizeof fingerprint, fp) == sizeof fingerprint);
    assert(fclose(fp) == 0);
    hirsel_embed_config config = {
        .size = sizeof config,
        .abi = HIRSEL_EMBED_ABI,
        .manifest_fp = {fingerprint, sizeof fingerprint},
        .n_place = 16777216,
        .client_bytes = 2 << 20,
        .start_timeout_ms = 10000,
        .grants = text(argv[3]),
        .dir_bases = text(""),
        .store_dir = text(argv[2]),
        .ended = runtime_ended,
    };
    hirsel_runtime rt;
    hirsel_status status = hirsel_embed_v1_start(&config, &rt);
    if (!strcmp(argv[4], "refused")) {
        assert(status == HIRSEL_REFUSED && rt.generation == 0);
        assert(ended == 1 && end_code == HIRSEL_END_REFUSED);
        if (!strstr(end_reason, argv[5])) {
            fprintf(stderr, "wanted refusal %s, got %s\n", argv[5], end_reason);
            return 3;
        }
        return 0;
    }
    if (status != HIRSEL_OK) {
        fprintf(stderr, "start returned %d (%s)\n", status, end_reason);
        return 2;
    }
    hirsel_embed_callbacks callbacks = {
        .size = sizeof callbacks,
        .outcome = outcome,
        .change = unused_event,
        .sub_ended = unused_event,
        .room_event = unused_event,
        .session_ended = session_ended,
    };
    const uint8_t credential[] = { 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16 };
    hirsel_session session;
    assert(hirsel_embed_v1_open(rt, &callbacks, (hirsel_bytes){credential, sizeof credential}, &session) == HIRSEL_OK);
    const uint8_t put[] = {7, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0};
    assert(hirsel_embed_v1_call(session, 1, 0, (hirsel_bytes){put, sizeof put}, 5000) == HIRSEL_OK);
    pthread_mutex_lock(&lock);
    while (!length) pthread_cond_wait(&delivered, &lock);
    pthread_mutex_unlock(&lock);
    assert(hirsel_embed_v1_stop(rt) == HIRSEL_OK);
    assert(ended == 1 && end_code == HIRSEL_END_STOPPED);
    struct sigaction action;
    assert(sigaction(SIGPIPE, NULL, &action) == 0 && action.sa_handler == SIG_DFL);
    assert(fwrite(answer, 1, length, stdout) == length);
    return 0;
}
