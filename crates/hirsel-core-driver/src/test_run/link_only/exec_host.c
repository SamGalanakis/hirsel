/* A C host for the embedded Exec laws (EMB-3 "Exec in an embedded launch").
 * With no arguments it prints the manifest. Otherwise argv is
 * fingerprint store grants helper mode, where mode is one of
 *   pipe:    a send to an exited child's stdin under SIGPIPE=SIG_DFL;
 *   state:   the host's subreaper flag, dumpable flag, SIGCHLD action and its
 *            own child survive a launch that spawns and releases children;
 *   death:   SIGKILL of the helper kills its child and ends the launch with
 *            the ExecSupervisor run stop;
 *   refused: start refuses; the reason is printed.
 * Every run holds an exclusive lock on ./exec_host.lock, taken by a
 * constructor before main, while start waits for the helper's READY: an
 * image that ran this host's initializers would block on it. */
#define _GNU_SOURCE
#include "hirsel_product.h"

#include <assert.h>
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/file.h>
#include <sys/prctl.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

static pthread_mutex_t lock = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t delivered = PTHREAD_COND_INITIALIZER;
static size_t length;
static uint8_t answer[64];
static volatile unsigned ended;
static uint32_t end_code;
static char end_reason[128];
static volatile sig_atomic_t chld_seen;
static int session_closed;

static hirsel_bytes text(const char *str) {
    return (hirsel_bytes){(const uint8_t *)str, strlen(str)};
}

_Noreturn void __wrap_exit(int status) {
    fprintf(stderr, "archive called exit(%d)\n", status);
    abort();
}

_Noreturn void __wrap__exit(int status) {
    fprintf(stderr, "archive called _exit(%d)\n", status);
    abort();
}

/* A single-instance lock of the kind host libraries take while they
 * initialize. It is held until the process ends. */
__attribute__((constructor)) static void single_instance(void) {
    int fd = open("exec_host.lock", O_RDWR | O_CREAT | O_CLOEXEC, 0600);
    if (fd < 0 || flock(fd, LOCK_EX) != 0) abort();
}

static void runtime_ended(void *context, uint32_t code, hirsel_bytes reason) {
    (void)context;
    assert(reason.len < sizeof end_reason);
    pthread_mutex_lock(&lock);
    if (reason.len) memcpy(end_reason, reason.ptr, reason.len);
    end_reason[reason.len] = '\0';
    end_code = code;
    ++ended;
    pthread_cond_broadcast(&delivered);
    pthread_mutex_unlock(&lock);
}

static void outcome(void *context, uint64_t id, hirsel_bytes bytes) {
    (void)context;
    assert(id == 1 && bytes.len && bytes.len <= sizeof answer);
    pthread_mutex_lock(&lock);
    memcpy(answer, bytes.ptr, bytes.len);
    length = bytes.len;
    pthread_cond_broadcast(&delivered);
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
    pthread_mutex_lock(&lock);
    session_closed = 1;
    pthread_cond_broadcast(&delivered);
    pthread_mutex_unlock(&lock);
}

static void on_chld(int sig) {
    (void)sig;
    chld_seen = 1;
}

/* The children of `pid`, from every thread's children list. */
static int children_of(pid_t pid, pid_t *out, int max) {
    char path[64];
    snprintf(path, sizeof path, "/proc/%d/task", (int)pid);
    DIR *tasks = opendir(path);
    if (!tasks) return 0;
    int n = 0;
    struct dirent *task;
    while ((task = readdir(tasks))) {
        if (task->d_name[0] == '.') continue;
        char file[128];
        snprintf(file, sizeof file, "%s/%s/children", path, task->d_name);
        FILE *f = fopen(file, "r");
        if (!f) continue;
        int child;
        while (n < max && fscanf(f, "%d", &child) == 1) out[n++] = child;
        fclose(f);
    }
    closedir(tasks);
    return n;
}

static void pause_ms(long ms) {
    struct timespec t = {ms / 1000, (ms % 1000) * 1000000L};
    nanosleep(&t, NULL);
}

/* Gone, or a zombie no one will run again. */
static int dead(pid_t pid) {
    char path[64], stat[256];
    snprintf(path, sizeof path, "/proc/%d/stat", (int)pid);
    FILE *f = fopen(path, "r");
    if (!f) return 1;
    size_t n = fread(stat, 1, sizeof stat - 1, f);
    fclose(f);
    stat[n] = '\0';
    char *close = strrchr(stat, ')');
    return !close || close[2] == 'Z' || close[2] == 'X';
}

int main(int argc, char **argv) {
    REQUIRE_ANCHOR();
    assert(hirsel_embed_v1_abi() == HIRSEL_EMBED_ABI);
    if (argc == 1) {
        hirsel_bytes manifest = hirsel_embed_v1_manifest();
        assert(fwrite(manifest.ptr, 1, manifest.len, stdout) == manifest.len);
        return 0;
    }
    assert(argc == 6);
    const char *mode = argv[5];
    assert(signal(SIGPIPE, SIG_DFL) != SIG_ERR);

    int subreaper = -1;
    int dumpable = prctl(PR_GET_DUMPABLE, 0, 0, 0, 0);
    assert(prctl(PR_GET_CHILD_SUBREAPER, &subreaper, 0, 0, 0) == 0);
    struct sigaction chld = {0};
    chld.sa_handler = on_chld;
    chld.sa_flags = SA_RESTART;
    sigemptyset(&chld.sa_mask);
    assert(sigaction(SIGCHLD, &chld, NULL) == 0);
    struct sigaction before;
    assert(sigaction(SIGCHLD, NULL, &before) == 0);
    pid_t own = -1;
    if (!strcmp(mode, "state")) {
        own = fork();
        assert(own >= 0);
        if (own == 0) {
            /* Dies with the host, so a failed run leaves nothing holding its pipes. */
            if (prctl(PR_SET_PDEATHSIG, SIGKILL, 0, 0, 0) != 0 || getppid() == 1) _Exit(1);
            for (;;) pause();
        }
    }

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
        .exec_helper = text(argv[4]),
        .ended = runtime_ended,
    };
    hirsel_runtime rt;
    hirsel_status status = hirsel_embed_v1_start(&config, &rt);
    if (!strcmp(mode, "refused")) {
        if (status != HIRSEL_REFUSED || ended != 1) {
            fprintf(stderr, "start returned %d, ended %u\n", status, ended);
            return 3;
        }
        fputs(end_reason, stdout);
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
    const uint8_t credential[] = {1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16};
    hirsel_session session;
    assert(hirsel_embed_v1_open(rt, &callbacks, (hirsel_bytes){credential, sizeof credential}, &session) == HIRSEL_OK);
    uint8_t arg[8] = {0};
    arg[0] = !strcmp(mode, "pipe") ? 0 : !strcmp(mode, "state") ? 1 : 2;
    assert(hirsel_embed_v1_call(session, 1, 0, (hirsel_bytes){arg, sizeof arg}, 60000) == HIRSEL_OK);

    if (!strcmp(mode, "death")) {
        pid_t kids[8], grandkids[8];
        pid_t helper = 0, child = 0;
        for (int tries = 0; tries < 1000 && !child; ++tries) {
            if (children_of(getpid(), kids, 8) == 1) {
                helper = kids[0];
                if (children_of(helper, grandkids, 8) == 1) child = grandkids[0];
            }
            if (!child) pause_ms(10);
        }
        if (!child) {
            fprintf(stderr, "no helper child appeared\n");
            return 4;
        }
        char comm[64] = {0};
        char path[64];
        snprintf(path, sizeof path, "/proc/%d/comm", (int)child);
        FILE *f = fopen(path, "r");
        assert(f && fgets(comm, sizeof comm, f));
        fclose(f);
        if (strncmp(comm, "sleep", 5)) {
            fprintf(stderr, "helper child is %s\n", comm);
            return 5;
        }
        assert(kill(helper, SIGKILL) == 0);
        int tries = 0;
        while (!dead(child) && tries++ < 1000) pause_ms(10);
        if (!dead(child)) {
            fprintf(stderr, "child %d outlived its helper\n", (int)child);
            kill(child, SIGKILL);
            return 6;
        }
        pthread_mutex_lock(&lock);
        while (!ended) pthread_cond_wait(&delivered, &lock);
        pthread_mutex_unlock(&lock);
        assert(hirsel_embed_v1_stop(rt) == HIRSEL_OK);
        if (end_code != HIRSEL_END_RUN_STOP || strcmp(end_reason, "ExecSupervisor")) {
            fprintf(stderr, "ended %u (%s)\n", end_code, end_reason);
            return 7;
        }
        int left = children_of(getpid(), kids, 8);
        if (left) {
            fprintf(stderr, "%d children left, first %d\n", left, (int)kids[0]);
            return 8;
        }
        fputs("ExecSupervisor", stdout);
        return 0;
    }

    pthread_mutex_lock(&lock);
    while (!length && !ended && !session_closed) pthread_cond_wait(&delivered, &lock);
    pthread_mutex_unlock(&lock);
    if (!length) {
        fprintf(stderr, "no outcome: %s (%u, %s)\n", ended ? "runtime ended" : "session closed",
                end_code, end_reason);
        return 14;
    }
    assert(hirsel_embed_v1_stop(rt) == HIRSEL_OK);
    assert(ended == 1 && end_code == HIRSEL_END_STOPPED);
    struct sigaction action;
    assert(sigaction(SIGPIPE, NULL, &action) == 0 && action.sa_handler == SIG_DFL);

    if (!strcmp(mode, "state")) {
        int now = -1;
        assert(prctl(PR_GET_CHILD_SUBREAPER, &now, 0, 0, 0) == 0);
        if (now != subreaper || prctl(PR_GET_DUMPABLE, 0, 0, 0, 0) != dumpable) {
            fprintf(stderr, "subreaper %d->%d, dumpable %d->%d\n", subreaper, now, dumpable,
                    prctl(PR_GET_DUMPABLE, 0, 0, 0, 0));
            return 9;
        }
        struct sigaction after;
        assert(sigaction(SIGCHLD, NULL, &after) == 0);
        if (after.sa_handler != before.sa_handler || after.sa_flags != before.sa_flags) {
            fprintf(stderr, "SIGCHLD action changed: flags %#x -> %#x\n", (unsigned)before.sa_flags,
                    (unsigned)after.sa_flags);
            return 10;
        }
        int st;
        if (waitpid(own, &st, WNOHANG) != 0) {
            fprintf(stderr, "the host's own child was reaped or lost\n");
            return 11;
        }
        pid_t kids[8];
        int n = children_of(getpid(), kids, 8);
        if (n != 1 || kids[0] != own) {
            fprintf(stderr, "%d children after stop\n", n);
            return 12;
        }
        assert(kill(own, SIGKILL) == 0);
        if (waitpid(own, &st, 0) != own || !WIFSIGNALED(st) || WTERMSIG(st) != SIGKILL) {
            fprintf(stderr, "the host could not reap its own child\n");
            return 13;
        }
    }
    assert(fwrite(answer, 1, length, stdout) == length);
    return 0;
}
