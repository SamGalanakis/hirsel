// Scope: only the ad-hoc signed arm64 fixture inherits this library.
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

int chaos_sync(int fd, int command) {
    struct stat st;
    int known = fstat(fd, &st) == 0;
    int rc = fcntl(fd, command);
    int saved = errno;
    dprintf(2, "FULLFSYNC %s %d %d\n",
            !known ? "unknown" : S_ISDIR(st.st_mode) ? "directory" : "file",
            rc, rc < 0 ? saved : 0);
    errno = saved;
    return rc;
}

// Forward other variadic calls with the caller's argument registers and stack intact.
extern int chaos_fcntl(int, int, ...);
_Static_assert(F_FULLFSYNC == 51, "arm64 Darwin F_FULLFSYNC");
__asm__(".text\n.globl _chaos_fcntl\n_chaos_fcntl:\n"
        "cmp w1, #51\nb.ne 1f\nb _chaos_sync\n1: b _fcntl\n");

static int chaos_rename(int from, const char *a, int to, const char *b) {
    const char *when = getenv("CHAOS_RENAME");
    int armed = when && !strcmp(b, "MANIFEST") && !strcmp(a, "MANIFEST.tmp");
    if (armed && !strcmp(when, "before")) {
        dprintf(2, "CUT before renameat MANIFEST.tmp MANIFEST\n");
        _exit(86);
    }
    int rc = renameat(from, a, to, b);
    int saved = errno;
    if (armed && rc == 0 && !strcmp(when, "after")) {
        dprintf(2, "CUT after renameat MANIFEST.tmp MANIFEST 0\n");
        _exit(86);
    }
    errno = saved;
    return rc;
}

__attribute__((used, section("__DATA,__interpose")))
static const struct { const void *replacement; const void *original; } hooks[] = {
    { (const void *)chaos_fcntl, (const void *)fcntl },
    { (const void *)chaos_rename, (const void *)renameat },
};
