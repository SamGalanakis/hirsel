#include "hirsel_core_rt.h"
#include <assert.h>
#include <stdint.h>
#include <stdlib.h>
#include <sys/mman.h>
#include <sys/resource.h>
#include <unistd.h>

@@DECLARATIONS@@
static uint8_t (*const bodies[])(HirselCoreContext *, void *, uint64_t@@SITE_PARAMETER@@) = {
    @@SYMBOLS@@
};
static uint8_t *old_payload;
static uint64_t *old_carrier;
void *__real_mmap(void *, size_t, int, int, int, off_t);
void *__wrap_mmap(void *address, size_t bytes, int protection, int flags, int fd, off_t offset) {
    void *result = __real_mmap(address, bytes, protection, flags, fd, offset);
    if (result == MAP_FAILED && old_payload) {
        /* A refused physical mapping leaves the uncommitted carrier intact. */
        assert(old_payload[0] == 42);
        assert(old_carrier[0] == (uintptr_t)old_payload);
        assert(old_carrier[1] == 1 && old_carrier[2] == 16384);
    }
    return result;
}
int main(int argc, char **argv) {
    assert(argc == 3);
    unsigned mode = (unsigned)strtoul(argv[1], NULL, 10);
    uint64_t bytes = strtoull(argv[2], NULL, 10);
    assert(mode < 10);
    HirselCoreContext *ctx = hirsel_core_context_new(UINT64_MAX);
    assert(hirsel_core_task_start(ctx).pointer);
    hirsel_core_task_bind(ctx);
    HirselCoreAllocation old = hirsel_core_alloc(ctx, NULL, 8, 8, 8, false, HIRSEL_CORE_NATIVE_PLAIN);
    assert(old.pointer);
    void *argument = old.pointer;
    unsigned body = mode;
    /* Native code holds a region as a slot: null until its first admitted allocation. */
    HirselCoreRegion *slot = NULL;
    if (mode == 1) {
        /* Fund the growth from a different current limit before testing host refusal. */
        assert(hirsel_core_limit_enter(ctx, bytes).pointer);
    } else if (mode == 2 || mode == 3) {
        if (mode == 3) {
            slot = hirsel_core_region_open(ctx);
            assert(hirsel_core_region_alloc(ctx, slot, 8, 8, 8, false).pointer);
        }
        argument = &slot;
        body = 2;
    } else if (mode >= 4) {
        body = mode - 1;
    }
    uint64_t carrier[3];
    /* This fixture stays above every measured crossover candidate. */
    if (mode >= 8) {
        HirselCoreAllocation buffer = hirsel_core_alloc(ctx, NULL, 16384, 8, 16384, false, HIRSEL_CORE_NATIVE_PLAIN);
        assert(buffer.pointer);
        ((uint8_t *)buffer.pointer)[0] = 42;
        carrier[0] = (uintptr_t)buffer.pointer; carrier[1] = 1; carrier[2] = 16384;
        argument = carrier; body = 7;
        if (mode == 9) assert(hirsel_core_limit_enter(ctx, bytes).pointer);
        old_payload = buffer.pointer; old_carrier = carrier;
    }
    struct rlimit denied = {0, 0};
    assert(setrlimit(RLIMIT_CORE, &denied) == 0);
    assert(setrlimit(RLIMIT_AS, &denied) == 0);
    /* Proves setup completed; this unbuffered write allocates nothing. */
    assert(write(1, "entered\n", 8) == 8);
    (void)bodies[body](ctx, argument, bytes@@SITE_ARGUMENT@@);
    (void)write(2, "RETURNED/CLEANUP\n", 17);
    return 80;
}
