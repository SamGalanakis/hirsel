#include "hirsel_core_rt.h"
#include <assert.h>
#include <string.h>

static bool empty(HirselCoreContext *context, void *payload, uint64_t *cursor,
                  HirselCoreWorklist *worklist) {
    (void)context; (void)payload; (void)cursor; (void)worklist;
    return true;
}

_Static_assert(sizeof(HirselCoreCause) == HIRSEL_CORE_CAUSE_BYTES, "Cause size");
_Static_assert(offsetof(HirselCoreCause, kind) == HIRSEL_CORE_CAUSE_KIND, "Cause kind");
_Static_assert(offsetof(HirselCoreCause, message) == HIRSEL_CORE_CAUSE_MESSAGE, "Cause message");
_Static_assert(offsetof(HirselCoreCause, site) == HIRSEL_CORE_CAUSE_SITE, "Cause site");

int main(void) {
    uint64_t destination = hirsel_core_dest_id();
    assert(destination != hirsel_core_dest_id());
    HirselCoreContext *context = hirsel_core_context_new(2048);
    void *limit = *(void **)((unsigned char *)context + HIRSEL_CORE_CONTEXT_CURRENT_LIMIT);
    uint64_t *room = (uint64_t *)((unsigned char *)limit + HIRSEL_CORE_LIMIT_AVAILABLE);
    assert(*room == 2048);
    assert(hirsel_core_task_start(context).pointer);
    assert(*room == 1760);
    /* Exact emitted own sequence: admission, native allocator, payer store. */
    uint64_t *outstanding = (uint64_t *)((unsigned char *)limit + HIRSEL_CORE_LIMIT_OUTSTANDING);
    assert(*room >= 8);
    *room -= 8; *outstanding += 8;
    unsigned char *base = hirsel_core_native_alloc(16, 8);
    *(void **)base = limit;
    uint64_t *inline_own = (uint64_t *)(base + HIRSEL_CORE_PLAIN_PREFIX_BYTES);
    *inline_own = 42;
    assert(*inline_own == 42);
    hirsel_core_native_free(base);
    hirsel_core_refund(limit, 8);
    assert(*room == 1760);
    HirselCoreAllocation entered = hirsel_core_limit_enter(context, 128);
    assert(entered.pointer);
    assert(entered.capacity.requested == 0 && entered.capacity.available == 0);
    HirselCoreAllocation own = hirsel_core_alloc(context, NULL, 8, 8, 8, false, 0);
    assert(own.pointer);
    assert(own.capacity.requested == 0 && own.capacity.available == 0);
    *(uint64_t *)own.pointer = 42;
    hirsel_core_limit_exit(context);
    HirselCoreAllocation aligned = hirsel_core_alloc(context, NULL, 17, 64, 24, false, 0);
    assert(aligned.pointer && (uintptr_t)aligned.pointer % 64 == 0);
    hirsel_core_free(aligned.pointer, 24);
    HirselCoreAllocation grown = hirsel_core_alloc(context, own.pointer, 16, 8, 16, false, 0);
    assert(grown.pointer);
    memcpy(grown.pointer, own.pointer, 8);
    assert(*(uint64_t *)grown.pointer == 42);
    hirsel_core_free(own.pointer, 8);
    hirsel_core_free(grown.pointer, 16);

    HirselCoreAllocation counted = hirsel_core_alloc(context, NULL, 16, 8, 16, false, 1);
    assert(counted.pointer);
    hirsel_core_counted_init(counted.pointer, false);
    *((uint64_t *)counted.pointer + 1) = 7;
    uint64_t *count = (uint64_t *)((unsigned char *)counted.pointer + HIRSEL_CORE_NODE_COUNT);
    assert(*count < UINT64_MAX);
    *count += 1;
    assert(!hirsel_core_try_unwrap(counted.pointer, false));
    assert(--*count != 0);
    assert(hirsel_core_try_unwrap(counted.pointer, false));
    if (--*count == 0) hirsel_core_release_last(context, counted.pointer, 16, empty);

    HirselCoreRegion *region = hirsel_core_region_open(128);
    HirselCoreAllocation stable = hirsel_core_region_alloc(context, region, 64, 16, 64, false);
    assert(stable.pointer);
    assert(*(uint64_t *)((unsigned char *)region + HIRSEL_CORE_REGION_CHARGE) == 64);
    assert(*(size_t *)((unsigned char *)region + HIRSEL_CORE_REGION_ENTRY) == 0);
    *(uint8_t *)stable.pointer = 73;
    hirsel_core_region_lower_charge(region, 0, 64, 8);
    assert(*(uint8_t *)stable.pointer == 73);
    hirsel_core_region_close(region);

    HirselCoreAllocation resource = hirsel_core_resource_alloc(context, 8, 8, 8, 64);
    assert(resource.pointer);
    hirsel_core_raise(context, (HirselCoreCause){ .kind = 0, .site = 99 });
    hirsel_core_finalizer_begin(context, resource.pointer);
    HirselCoreAllocation refusal = hirsel_core_alloc(context, NULL, 128, 8, 128, false, 0);
    assert(!refusal.pointer);
    assert(refusal.capacity.requested == 128 && refusal.capacity.available == 64);
    hirsel_core_raise(context, (HirselCoreCause){ .kind = 6, .site = 100 });
    hirsel_core_finalizer_end(context, resource.pointer);
    hirsel_core_resource_free(resource.pointer, 8);
    HirselCoreCauseCell *causes = hirsel_core_causes_take(context);
    hirsel_core_context_free(context);
    assert(hirsel_core_cause_value(causes)->site == 99);
    const HirselCoreCauseCell *next = hirsel_core_cause_next(causes);
    assert(hirsel_core_cause_value(next)->site == 100);
    assert(!hirsel_core_cause_next(next));
    hirsel_core_causes_drop(causes);
    return 0;
}
