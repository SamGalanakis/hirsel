/* Loopback qualification only: force prefix sends without changing global
 * kernel settings or the production provider. All I/O remains real Linux TCP. */
#define _GNU_SOURCE
#include <dlfcn.h>
#include <sys/socket.h>

int accept4(int fd, struct sockaddr *address, socklen_t *length, int flags) {
    int (*real_accept4)(int, struct sockaddr *, socklen_t *, int) =
        dlsym(RTLD_NEXT, "accept4");
    int accepted = real_accept4(fd, address, length, flags);
    if (accepted >= 0) {
        int size = 4096;
        if (setsockopt(accepted, SOL_SOCKET, SO_SNDBUF, &size, sizeof(size)) != 0) {
            __builtin_trap();
        }
    }
    return accepted;
}

#ifdef WIRE_CUT_OUTCOME
#include <errno.h>
#include <stdatomic.h>
#include <string.h>

static atomic_bool cut;

// The first complete outcome-only Batch was built from a durable answer.
// Fail before writing any of its bytes so the owner cannot observe Sent.
ssize_t send(int fd, const void *buffer, size_t length, int flags) {
    ssize_t (*real_send)(int, const void *, size_t, int) = dlsym(RTLD_NEXT, "send");
    const unsigned char *bytes = buffer;
    if (length >= 26 && bytes[0] == 0x82 && bytes[1] < 126
        && memcmp(bytes + 2, "\x06\0\0\0", 4) == 0
        && memcmp(bytes + 14, "\0\0\0\0\0\0\0\0\x01\0\0\0", 12) == 0
        && !atomic_exchange(&cut, 1)) {
        errno = EPIPE;
        return -1;
    }
    return real_send(fd, buffer, length, flags);
}
#endif
