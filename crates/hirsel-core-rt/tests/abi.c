#include "hirsel_core_rt.h"
#include <assert.h>
#include <string.h>

static bool empty(HirselCoreContext *context, void *payload, uint64_t *cursor,
                  HirselCoreWorklist *worklist) {
    (void)context; (void)payload; (void)cursor; (void)worklist;
    return true;
}

int main(void) {
    HirselCoreContext *context = hirsel_core_context_new(2048);
    assert(hirsel_core_task_start(context).pointer);
    HirselCoreAllocation entered = hirsel_core_limit_enter(context, 128);
    assert(entered.pointer);
    assert(entered.capacity.requested == 0 && entered.capacity.available == 0);
    HirselCoreAllocation own = hirsel_core_alloc(context, NULL, 8, 8, 8, false);
    assert(own.pointer);
    assert(own.capacity.requested == 0 && own.capacity.available == 0);
    *(uint64_t *)own.pointer = 42;
    hirsel_core_limit_exit(context);
    HirselCoreAllocation grown = hirsel_core_alloc(context, own.pointer, 16, 8, 16, false);
    assert(grown.pointer);
    memcpy(grown.pointer, own.pointer, 8);
    assert(*(uint64_t *)grown.pointer == 42);
    hirsel_core_free(own.pointer, 8);
    hirsel_core_free(grown.pointer, 16);

    HirselCoreAllocation counted = hirsel_core_alloc(context, NULL, 16, 8, 16, false);
    assert(counted.pointer);
    hirsel_core_counted_init(counted.pointer, false);
    *((uint64_t *)counted.pointer + 1) = 7;
    hirsel_core_share(counted.pointer, false);
    assert(!hirsel_core_try_unwrap(counted.pointer, false));
    hirsel_core_release(context, counted.pointer, false, 16, empty);
    assert(hirsel_core_try_unwrap(counted.pointer, false));
    hirsel_core_release(context, counted.pointer, false, 16, empty);

    HirselCoreRegion *region = hirsel_core_region_open(128);
    HirselCoreAllocation stable = hirsel_core_region_alloc(context, region, 64, 16, 64, false);
    assert(stable.pointer);
    *(uint8_t *)stable.pointer = 73;
    hirsel_core_region_lower_charge(stable.pointer, 8);
    assert(*(uint8_t *)stable.pointer == 73);
    hirsel_core_region_close(region);

    HirselCoreAllocation resource = hirsel_core_resource_alloc(context, 8, 8, 8, 64);
    assert(resource.pointer);
    hirsel_core_raise(context, (HirselCoreCause){ .kind = 0, .site = 99 });
    hirsel_core_finalizer_begin(context, resource.pointer);
    HirselCoreAllocation refusal = hirsel_core_alloc(context, NULL, 128, 8, 128, false);
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
