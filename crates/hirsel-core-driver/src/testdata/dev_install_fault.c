#define _GNU_SOURCE
#include <dlfcn.h>
#include <fcntl.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <sys/file.h>
#include <unistd.h>

static bool install_process(void) {
    int fd = open("/proc/self/cmdline", O_RDONLY);
    char args[8192];
    if (fd < 0) return false;
    ssize_t len = read(fd, args, sizeof(args));
    close(fd);
    if (len <= 0) return false;
    return memmem(args, (size_t)len, "--hirsel-install-predecessor=", 29) != NULL;
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
    for (;;) pause();
}

int flock(int fd, int operation) {
    int (*next)(int, int) = dlsym(RTLD_NEXT, "flock");
    int result = next(fd, operation);
    if (result == 0 && (operation & LOCK_EX) != 0 && install_process())
        interrupt_at("before");
    return result;
}

ssize_t write(int fd, const void *data, size_t len) {
    ssize_t (*next)(int, const void *, size_t) = dlsym(RTLD_NEXT, "write");
    // The install reply is the first output following store close.
    static const unsigned char installed[] = {5, 0, 0, 0};
    if (len == 13 && memcmp(data, installed, sizeof(installed)) == 0 && install_process())
        interrupt_at("after");
    return next(fd, data, len);
}

int rename(const char *from, const char *to) {
    int (*next)(const char *, const char *) = dlsym(RTLD_NEXT, "rename");
    const char *name = strrchr(to, '/');
    if (name && strcmp(name, "/state.json") == 0)
        interrupt_at("state");
    return next(from, to);
}
