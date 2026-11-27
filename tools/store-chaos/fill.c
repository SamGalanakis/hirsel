// Pause at the selected userspace call and fill this cell's mounted volume.
// The kernel executes the original operation and supplies its real result.
#define _GNU_SOURCE
#define _LARGEFILE64_SOURCE
#include <dlfcn.h>
#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/uio.h>
#include <unistd.h>

static ssize_t (*real_write_fn(void))(int, const void *, size_t) {
    static ssize_t (*fn)(int, const void *, size_t);
    if (!fn) fn = dlsym(RTLD_NEXT, "write");
    return fn;
}

static void fill(const char *site) {
    static int filled;
    const char *target = getenv("CHAOS_FILL_SITE");
    if (!target || strcmp(target, site) || access(getenv("CHAOS_ARM"), F_OK) || __atomic_exchange_n(&filled, 1, __ATOMIC_SEQ_CST)) return;
    int fd = open(getenv("CHAOS_FILL_PATH"), O_WRONLY | O_CREAT | O_EXCL, 0600);
    if (fd < 0) _exit(90);
    static const char block[65536];
    ssize_t (*rw)(int, const void *, size_t) = real_write_fn();
    ssize_t n;
    for (size_t chunk = sizeof(block); chunk >= 256; chunk /= 4) {
        while ((n = rw(fd, block, chunk)) > 0) {}
        if (n < 0 && errno != ENOSPC) _exit(91);
    }
    fsync(fd);
    close(fd);
}

static void before_write(int fd, int vector, off64_t offset) {
    char proc[64], path[PATH_MAX];
    snprintf(proc, sizeof(proc), "/proc/self/fd/%d", fd);
    ssize_t n = readlink(proc, path, sizeof(path) - 1);
    if (n < 0) return;
    path[n] = 0;
    const char *name = strrchr(path, '/');
    if (!name) return;
    if (vector && !strncmp(name, "/j-", 3)) fill("journal");
    if (!vector && offset >= 4096 && !strncmp(name, "/j-", 3)) fill("journal-pwrite");
    if (!strcmp(name, "/MANIFEST.tmp")) fill("manifest");
    if (!strncmp(name, "/p-", 3)) fill("pack");
}

ssize_t write(int fd, const void *p, size_t n) {
    before_write(fd, 0, -1);
    return real_write_fn()(fd, p, n);
}

#define WRITE_WRAPPER(NAME, TYPE) \
ssize_t NAME(int fd, const void *p, size_t n, TYPE offset) { \
    ssize_t (*real)(int, const void *, size_t, TYPE) = dlsym(RTLD_NEXT, #NAME); \
    before_write(fd, 0, (off64_t)offset); return real(fd, p, n, offset); \
}

WRITE_WRAPPER(pwrite, off_t)
WRITE_WRAPPER(pwrite64, off64_t)

#define VECTOR_WRAPPER(NAME, TYPE) \
ssize_t NAME(int fd, const struct iovec *v, int n, TYPE offset) { \
    ssize_t (*real)(int, const struct iovec *, int, TYPE) = dlsym(RTLD_NEXT, #NAME); \
    before_write(fd, 1, (off64_t)offset); return real(fd, v, n, offset); \
}

VECTOR_WRAPPER(pwritev, off_t)
VECTOR_WRAPPER(pwritev64, off64_t)

int renameat(int from, const char *a, int to, const char *b) {
    int (*real)(int, const char *, int, const char *) = dlsym(RTLD_NEXT, "renameat");
    if (!strcmp(b, "MANIFEST")) fill("rename");
    return real(from, a, to, b);
}
