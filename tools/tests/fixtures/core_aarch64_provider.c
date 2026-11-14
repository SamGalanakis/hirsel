#define _GNU_SOURCE
#include <assert.h>
#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/syscall.h>
#include <unistd.h>
#include "hirsel_core_rt.h"

/* Linux AArch64 UAPI. The pinned cross sysroot predates openat2 headers. */
struct probe_open_how { uint64_t flags, mode, resolve; };
enum { ARM_OPENAT2 = 437, PROBE_BENEATH = 0x08, PROBE_NO_MAGICLINKS = 0x02 };

static uint8_t entry(HirselCoreContext *ctx, const HirselCoreProcessConfig *cfg,
                     HirselCoreMainResult *out) {
    assert(cfg->grants_len == 1);
    HirselCoreBuffer path = {(uint8_t *)"provider.txt", 12, 0};
    HirselCoreHostOutcome opened = hirsel_core_host_op(ctx, 0, cfg->grants[0], &path, 1, 0);
    assert(!opened.status && !opened.error.tag);
    HirselCoreBuffer message = {(uint8_t *)"hello", 5, 0};
    HirselCoreHostOutcome wrote = hirsel_core_host_op(ctx, 2, opened.value, &message, 0, 0);
    assert(!wrote.status && !wrote.error.tag && wrote.written == 5);
    HirselCoreHostOutcome closed = hirsel_core_host_op(ctx, 3, opened.value, NULL, 0, 0);
    assert(!closed.status && !closed.error.tag);
    opened = hirsel_core_host_op(ctx, 0, cfg->grants[0], &path, 0, 0);
    assert(!opened.status && !opened.error.tag);
    uint8_t bytes[8] = {0};
    HirselCoreBuffer buffer = {bytes, 0, sizeof bytes};
    HirselCoreHostOutcome read = hirsel_core_host_op(ctx, 1, opened.value, &buffer, 0, 0);
    assert(!read.status && !read.error.tag && buffer.len == 5);
    assert(memcmp(bytes, "hello", 5) == 0);
    hirsel_core_hostfile_drop(ctx, opened.value);
    out->kind = 0;
    out->exit = 0;
    return 0;
}

int main(int argc, char **argv) {
    if (argc == 2 && strcmp(argv[1], "probe") == 0) {
        /* The same syscall/resolve flags as 907's host qualification probe. */
        struct probe_open_how how = {.flags = O_PATH | O_DIRECTORY | O_CLOEXEC,
                              .resolve = PROBE_BENEATH | PROBE_NO_MAGICLINKS};
        long fd = syscall(ARM_OPENAT2, AT_FDCWD, ".", &how, sizeof how);
        printf("openat2=%ld errno=%d\n", fd, errno);
        if (fd < 0) return 1;
        close((int)fd);
        return 0;
    }
    assert(argc == 1);
    HirselCoreView directory = {(const uint8_t *)".", 1};
    HirselCoreView name = {(const uint8_t *)"dir", 3};
    HirselCoreEntryRequirement parameter = {0, 0};
    HirselCoreOsConfig config = {.root_limit = 65536,
                                .directories = &directory, .directories_len = 1,
                                .stdout_fd = 1, .stderr_fd = 2,
                                .parameters = &parameter, .parameters_len = 1};
    uint8_t status = hirsel_core_os_run(&config, &name, entry);
    assert(status == 0);
    puts("provider=532");
    return 0;
}
