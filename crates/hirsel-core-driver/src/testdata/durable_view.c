// Logs what each successful durability call makes durable, for a power-cut
// model of one directory entry. A sync of the directory DURABLE_DIR logs
// "dir <inode>", the inode its entry DURABLE_NAME names at that moment (0 when
// absent). A sync of any regular file logs "file <inode>".
#define _GNU_SOURCE
#include <dlfcn.h>
#include <errno.h>
#include <fcntl.h>
#include <inttypes.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

static void record(int fd) {
    const char *log_path = getenv("DURABLE_LOG");
    const char *dir = getenv("DURABLE_DIR");
    const char *name = getenv("DURABLE_NAME");
    struct stat info, watched;
    if (!log_path || !dir || !name || fstat(fd, &info) != 0) return;
    char line[64];
    int length = 0;
    if (S_ISREG(info.st_mode)) {
        length = snprintf(line, sizeof(line), "file %ju\n", (uintmax_t)info.st_ino);
    } else if (S_ISDIR(info.st_mode) && stat(dir, &watched) == 0 &&
               watched.st_dev == info.st_dev && watched.st_ino == info.st_ino) {
        struct stat entry;
        uintmax_t inode = fstatat(fd, name, &entry, AT_SYMLINK_NOFOLLOW) == 0 ? entry.st_ino : 0;
        length = snprintf(line, sizeof(line), "dir %ju\n", inode);
    }
    if (length <= 0) return;
    int log = open(log_path, O_WRONLY | O_APPEND | O_CREAT | O_CLOEXEC, 0600);
    if (log < 0 || write(log, line, (size_t)length) != length) _exit(125);
    close(log);
}

#ifdef __APPLE__
__attribute__((used, visibility("hidden"))) int durable_view_full_sync(int fd) {
    int result = fcntl(fd, F_FULLFSYNC);
    int saved = errno;
    if (result == 0) record(fd);
    errno = saved;
    return result;
}

// A tail branch keeps the original variadic arguments for other commands.
#define STRINGIFY_(value) #value
#define STRINGIFY(value) STRINGIFY_(value)
__attribute__((naked)) static int observed_fcntl(int fd, int command, ...) {
    __asm__("cmp w1, #" STRINGIFY(F_FULLFSYNC) "\n"
            "b.ne 1f\n"
            "b _durable_view_full_sync\n"
            "1: b _fcntl");
}

__attribute__((used, section("__DATA,__interpose")))
static const struct { int (*replacement)(int, int, ...), (*original)(int, int, ...); }
    interpose = {observed_fcntl, fcntl};
#else
#define WRAP(name)                                         \
    int name(int fd) {                                     \
        int (*real)(int) = dlsym(RTLD_NEXT, #name);        \
        int result = real(fd);                             \
        int saved = errno;                                 \
        if (result == 0) record(fd);                       \
        errno = saved;                                     \
        return result;                                     \
    }
WRAP(fsync)
WRAP(fdatasync)
#endif
