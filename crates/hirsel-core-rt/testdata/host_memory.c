#include "hirsel_core_rt.h"
#include <assert.h>
#include <stdint.h>
#include <stdlib.h>
#include <sys/resource.h>
#include <unistd.h>

@@DECLARATIONS@@
static uint8_t (*const bodies[])(HirselCoreContext *, void *, uint64_t@@SITE_PARAMETER@@) = {
    @@SYMBOLS@@
};
static uint8_t *realloc_payload;
static uint64_t *realloc_carrier;
void *__real_realloc(void *, size_t);
void *__wrap_realloc(void *base, size_t bytes) {
    void *result = __real_realloc(base, bytes);
    if (!result && realloc_payload) {
        /* A real malloc refusal leaves the old payload and the uncommitted
           carrier intact before the runtime reports its no-cleanup stop. */
        assert(realloc_payload[0] == 42);
        assert(realloc_carrier[0] == (uintptr_t)realloc_payload);
        assert(realloc_carrier[1] == 1 && realloc_carrier[2] == 1024);
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
    if (mode == 1) {
        /* Growth must use the old block's root payer, not this empty scope. */
        assert(hirsel_core_limit_enter(ctx, 0).pointer);
    } else if (mode == 2 || mode == 3) {
        HirselCoreRegion *region = hirsel_core_region_open(4096);
        if (mode == 3) assert(hirsel_core_region_alloc(ctx, region, 8, 8, 8, false).pointer);
        argument = region;
        body = 2;
    } else if (mode >= 4) {
        body = mode - 1;
    }
    uint64_t carrier[3];
    if (mode >= 8) {
        HirselCoreAllocation buffer = hirsel_core_alloc(ctx, NULL, 1024, 8, 1024, false, HIRSEL_CORE_NATIVE_PLAIN);
        assert(buffer.pointer);
        ((uint8_t *)buffer.pointer)[0] = 42;
        carrier[0] = (uintptr_t)buffer.pointer; carrier[1] = 1; carrier[2] = 1024;
        argument = carrier; body = 7;
        if (mode == 9) assert(hirsel_core_limit_enter(ctx, 0).pointer);
        realloc_payload = buffer.pointer; realloc_carrier = carrier;
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
