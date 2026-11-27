#define _GNU_SOURCE
#include <dlfcn.h>
#include <fcntl.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/file.h>
#include <unistd.h>

#ifdef __APPLE__
#include <crt_externs.h>
#define OBSERVE(name) observe_##name
#define REAL(name, type) name
#else
#define OBSERVE(name) name
#define REAL(name, type) ((type)dlsym(RTLD_NEXT, #name))
#endif

static bool install_process(void) {
#ifdef __APPLE__
    const char prefix[] = "--hirsel-install-predecessor=";
    char **args = *_NSGetArgv();
    for (int i = 1; i < *_NSGetArgc(); ++i)
        if (strncmp(args[i], prefix, sizeof(prefix) - 1) == 0) return true;
    return false;
#else
    int fd = open("/proc/self/cmdline", O_RDONLY);
    char args[8192];
    if (fd < 0) return false;
    ssize_t len = read(fd, args, sizeof(args));
    close(fd);
    if (len <= 0) return false;
    return memmem(args, (size_t)len, "--hirsel-install-predecessor=", 29) != NULL;
#endif
}

static void interrupt_at(const char *step) {
    const char *fault = getenv("HIRSEL_TEST_DEV_FAULT");
    const char *marker = getenv("HIRSEL_TEST_DEV_MARKER");
    if (!fault || !marker) return;
    bool hold = strncmp(fault, "hold-", 5) == 0;
    if ((!hold && strncmp(fault, "exit-", 5) != 0) || strcmp(fault + 5, step) != 0) return;
    int fd = open(marker, O_WRONLY | O_CREAT | O_EXCL, 0600);
    if (fd < 0) _exit(120);
    close(fd);
    if (!hold) _exit(121);
#ifdef __APPLE__
    // Darwin has no PR_SET_PDEATHSIG; match Linux parent-death termination
    // when the test SIGKILLs the supervising `hirsel dev` process.
    pid_t parent = getppid();
    if (parent <= 1) _exit(121);
    while (getppid() == parent) usleep(10000);
    _exit(121);
#else
    for (;;) pause();
#endif
}

int OBSERVE(flock)(int fd, int operation) {
    int (*next)(int, int) = REAL(flock, int (*)(int, int));
    int result = next(fd, operation);
    if (result == 0 && (operation & LOCK_EX) != 0 && install_process())
        interrupt_at("before");
    return result;
}

ssize_t OBSERVE(write)(int fd, const void *data, size_t len) {
    ssize_t (*next)(int, const void *, size_t) = REAL(write, ssize_t (*)(int, const void *, size_t));
    // The install reply is the first output following store close.
    static const unsigned char installed[] = {5, 0, 0, 0};
    if (len == 13 && memcmp(data, installed, sizeof(installed)) == 0 && install_process())
        interrupt_at("after");
    return next(fd, data, len);
}

int OBSERVE(rename)(const char *from, const char *to) {
    int (*next)(const char *, const char *) = REAL(rename, int (*)(const char *, const char *));
    const char *name = strrchr(to, '/');
    if (name && strcmp(name, "/state.json") == 0)
        interrupt_at("state");
    return next(from, to);
}

#ifdef __APPLE__
__attribute__((used, section("__DATA,__interpose")))
static const struct { const void *replacement, *original; } hooks[] = {
    {observe_flock, flock}, {observe_write, write}, {observe_rename, rename},
};
#endif
