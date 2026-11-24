#define _GNU_SOURCE
#include <arpa/inet.h>
#include <endian.h>
#include <errno.h>
#include <inttypes.h>
#include <linux/udp.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <time.h>
#include <unistd.h>

enum { FLOWS = 2000, BYTES = 200 };
static const uint64_t INTERVAL_NS = 10000;

static uint64_t monotonic_ns(void)
{
    struct timespec now;
    if (clock_gettime(CLOCK_MONOTONIC, &now) != 0) {
        perror("clock_gettime");
        exit(1);
    }
    return (uint64_t)now.tv_sec * 1000000000 + (uint64_t)now.tv_nsec;
}

static void wait_until(uint64_t target)
{
    while (monotonic_ns() < target) {
    }
}

static void gso_sender(int fd, uint64_t start, uint64_t count)
{
    enum { PACKET = 1200, FIRST = 54, SECOND = 10 };
    unsigned char bytes[(FIRST + SECOND) * PACKET] = { 0 };
    struct iovec buffers[2] = {
        { .iov_base = bytes, .iov_len = FIRST * PACKET },
        { .iov_base = bytes + FIRST * PACKET, .iov_len = SECOND * PACKET },
    };
    union {
        struct cmsghdr align;
        unsigned char bytes[CMSG_SPACE(sizeof(uint16_t))];
    } control[2] = { 0 };
    struct mmsghdr trains[2] = { 0 };
    for (size_t i = 0; i < 2; i++) {
        struct msghdr *msg = &trains[i].msg_hdr;
        msg->msg_iov = &buffers[i];
        msg->msg_iovlen = 1;
        msg->msg_control = control[i].bytes;
        msg->msg_controllen = sizeof(control[i].bytes);
        struct cmsghdr *cmsg = CMSG_FIRSTHDR(msg);
        cmsg->cmsg_level = IPPROTO_UDP;
        cmsg->cmsg_type = UDP_SEGMENT;
        cmsg->cmsg_len = CMSG_LEN(sizeof(uint16_t));
        uint16_t segment = PACKET;
        memcpy(CMSG_DATA(cmsg), &segment, sizeof(segment));
    }
    for (uint64_t sent = 0; sent < count; sent += FIRST + SECOND) {
        wait_until(start + sent * 1000);
        uint64_t stamp = htole64(monotonic_ns());
        for (size_t i = 0; i < FIRST + SECOND; i++) {
            memcpy(bytes + i * PACKET, &stamp, sizeof(stamp));
        }
        unsigned remaining = 2;
        while (remaining != 0) {
            int committed = sendmmsg(fd, trains + 2 - remaining, remaining, MSG_DONTWAIT);
            if (committed <= 0) {
                fprintf(stderr, "UDP GSO at packet %" PRIu64 ", %u trains left: %s\n",
                        sent, remaining, strerror(errno));
                exit(1);
            }
            remaining -= (unsigned)committed;
        }
    }
}

int main(int argc, char **argv)
{
    if (argc != 5 || (strcmp(argv[1], "s1") != 0 && strcmp(argv[1], "s2") != 0)) {
        return 2;
    }
    int gso = strcmp(argv[1], "s1") == 0;
    uint16_t port = (uint16_t)strtoul(argv[2], NULL, 10);
    uint64_t start = strtoull(argv[3], NULL, 10);
    uint64_t count = strtoull(argv[4], NULL, 10);
    if (gso && count % 64 != 0) {
        return 2;
    }
    struct sockaddr_in target = {
        .sin_family = AF_INET,
        .sin_port = htons(port),
        .sin_addr = { htonl(INADDR_LOOPBACK) },
    };
    int connections[FLOWS];
    for (size_t flow = 0; flow < (gso ? 1 : FLOWS); flow++) {
        struct sockaddr_in source = {
            .sin_family = AF_INET,
            .sin_port = htons((uint16_t)(10000 + flow)),
            .sin_addr = { htonl(INADDR_LOOPBACK + 1) },
        };
        int fd = socket(AF_INET, SOCK_DGRAM | SOCK_CLOEXEC, IPPROTO_UDP);
        if (fd < 0 || bind(fd, (struct sockaddr *)&source, sizeof(source)) != 0
            || connect(fd, (struct sockaddr *)&target, sizeof(target)) != 0) {
            perror("open UDP flow");
            return 1;
        }
        connections[flow] = fd;
    }
    puts("READY");
    fflush(stdout);
    if (monotonic_ns() >= start) {
        fputs("flow setup missed the start time\n", stderr);
        return 1;
    }
    if (gso) {
        gso_sender(connections[0], start, count);
        printf("SENT %" PRIu64 " %" PRIu64 "\n", count, monotonic_ns() - start);
        return 0;
    }
    unsigned char packet[BYTES] = { 0 };
    for (uint64_t seq = 0; seq < count; seq++) {
        wait_until(start + seq * INTERVAL_NS);
        uint64_t stamp = htole64(monotonic_ns());
        uint64_t sequence = htole64(seq);
        memcpy(packet, &stamp, sizeof(stamp));
        memcpy(packet + sizeof(stamp), &sequence, sizeof(sequence));
        int fd = connections[seq % FLOWS];
        if (send(fd, packet, sizeof(packet), MSG_DONTWAIT) != sizeof(packet)) {
            perror("send UDP packet");
            return 1;
        }
    }
    printf("SENT %" PRIu64 " %" PRIu64 "\n", count, monotonic_ns() - start);
    return 0;
}
