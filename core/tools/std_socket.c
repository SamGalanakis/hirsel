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
