/* MEM-16 native harness: symbolic MAX-1/MAX counts, not a source API or measured workload.
 * Synthetic holders avoid executing 2^64 shares. The payload is an owned scalar. */
#include "hirsel_core_rt.h"
#include <assert.h>
#include <stdatomic.h>
#include <stdio.h>
#include <string.h>
static bool leaf(HirselCoreContext *ctx, void *payload, uint64_t *cursor,
                 HirselCoreWorklist *work) {
    (void)ctx; (void)payload; (void)cursor; (void)work; return true;
}
int main(int argc, char **argv) {
    assert(argc == 3);
    bool atomic = strcmp(argv[1], "arc") == 0;
    bool overflow = strcmp(argv[2], "overflow") == 0;
    HirselCoreContext *ctx = hirsel_core_context_new(4096);
    HirselCoreAllocation alloc = hirsel_core_alloc(ctx, NULL, 16, 8, 32, false,
                                                  HIRSEL_CORE_NATIVE_GRAPH);
    assert(alloc.pointer);
    hirsel_core_counted_init(alloc.pointer, atomic);
    uint64_t count = overflow ? UINT64_MAX : UINT64_MAX - 1;
    if (atomic) atomic_store((_Atomic uint64_t *)alloc.pointer, count);
    else *(uint64_t *)alloc.pointer = count;
    hirsel_core_share(alloc.pointer, atomic);
    /* Overflow must stop before this point. MAX-1 share must reach MAX exactly. */
    assert(!overflow);
    assert(atomic ? atomic_load((_Atomic uint64_t *)alloc.pointer) == UINT64_MAX
                  : *(uint64_t *)alloc.pointer == UINT64_MAX);
    /* Remove synthetic holders; release the one actual owning harness holder. */
    if (atomic) atomic_store((_Atomic uint64_t *)alloc.pointer, 1);
    else *(uint64_t *)alloc.pointer = 1;
    hirsel_core_release(ctx, alloc.pointer, atomic, 32, leaf);
    hirsel_core_context_free(ctx);
    puts("1");
    return 0;
}
