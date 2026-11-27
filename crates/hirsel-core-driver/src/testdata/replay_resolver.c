#include <netdb.h>
#include <netinet/in.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#ifdef __APPLE__
#define __wrap_getaddrinfo getaddrinfo
#define __wrap_freeaddrinfo freeaddrinfo
#endif

int __wrap_getaddrinfo(const char *name, const char *service,
                       const struct addrinfo *hints, struct addrinfo **out) {
    if (getenv("HIRSEL_NET11_OFFLINE")) {
        (void)write(2, "offline resolver called\n", 24);
        return EAI_AGAIN;
    }
    if (strcmp(name, "DUP.TEST.") != 0 || service ||
        !hints || hints->ai_socktype != SOCK_STREAM)
        return EAI_FAIL;
    static struct sockaddr_in6 v6 = { .sin6_family = AF_INET6, .sin6_addr = IN6ADDR_LOOPBACK_INIT };
    static struct sockaddr_in v4 = { .sin_family = AF_INET };
    static struct addrinfo answer[3];
    v4.sin_addr.s_addr = htonl(0xc0000211);
    answer[0] = (struct addrinfo){ .ai_family = AF_INET6, .ai_addr = (struct sockaddr *)&v6,
                                 .ai_addrlen = sizeof(v6), .ai_next = &answer[1] };
    answer[1] = answer[0];
    answer[1].ai_next = &answer[2];
    answer[2] = (struct addrinfo){ .ai_family = AF_INET, .ai_addr = (struct sockaddr *)&v4,
                                 .ai_addrlen = sizeof(v4) };
    *out = answer;
    (void)write(2, "resolver called\n", 16);
    return 0;
}
void __wrap_freeaddrinfo(struct addrinfo *answer) { (void)answer; }
