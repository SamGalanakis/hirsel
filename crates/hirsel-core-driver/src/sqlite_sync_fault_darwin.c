// sqlite_directory_recovery_locking_and_sync uses this in its arm64 Mac child.
// Only F_FULLFSYNC of a directory fails. The tail branch forwards all other
// fcntl calls with the caller's variadic arguments still in place.
#include <errno.h>
#include <fcntl.h>
#include <sys/stat.h>

__attribute__((used, visibility("hidden"))) int sqlite_test_full_sync(int fd) {
    struct stat info;
    if (fstat(fd, &info) == 0 && S_ISDIR(info.st_mode)) {
        errno = EIO;
        return -1;
    }
    return fcntl(fd, F_FULLFSYNC);
}

#define STRINGIFY_(x) #x
#define STRINGIFY(x) STRINGIFY_(x)
__attribute__((naked)) static int test_fcntl(int fd, int command, ...) {
    __asm__("cmp w1, #" STRINGIFY(F_FULLFSYNC) "\n"
            "b.ne 1f\n"
            "b _sqlite_test_full_sync\n"
            "1: b _fcntl");
}

__attribute__((used, section("__DATA,__interpose")))
static const struct { int (*replacement)(int, int, ...), (*original)(int, int, ...); }
    hook = {test_fcntl, fcntl};
