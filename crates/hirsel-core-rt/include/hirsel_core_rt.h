#ifndef HIRSEL_CORE_RT_H
#define HIRSEL_CORE_RT_H
#include <stdint.h>
#include <stddef.h>
#include <stdbool.h>
typedef struct HirselCoreContext HirselCoreContext;
typedef struct { uint64_t requested, available; } HirselCoreCapacity;
typedef struct { void *pointer; HirselCoreCapacity capacity; } HirselCoreAllocation;
HirselCoreContext *hirsel_core_context_new(uint64_t bytes);
void hirsel_core_context_free(HirselCoreContext *);
HirselCoreAllocation hirsel_core_alloc(const HirselCoreContext *, void *like,
    size_t bytes, size_t alignment, uint64_t charge, bool charge_overflow);
void hirsel_core_free(void *, uint64_t remaining_charge);
HirselCoreAllocation hirsel_core_limit_enter(HirselCoreContext *, uint64_t bytes);
void hirsel_core_limit_exit(HirselCoreContext *);
typedef struct HirselCoreRegion HirselCoreRegion;
HirselCoreRegion *hirsel_core_region_open(size_t physical_chunk_bytes);
HirselCoreAllocation hirsel_core_region_alloc(const HirselCoreContext *,
    HirselCoreRegion *, size_t bytes, size_t alignment, uint64_t charge, bool charge_overflow);
void hirsel_core_region_lower_charge(void *, uint64_t retained_charge);
void hirsel_core_region_close(HirselCoreRegion *);
typedef struct HirselCoreWorklist HirselCoreWorklist;
/* Resumable glue: cursor starts at 0. Advance it, enqueue at most one child,
   return false; return true only after all children and fields are cleaned. */
typedef bool (*HirselCoreDropRecipe)(HirselCoreContext *, void *payload, uint64_t *cursor, HirselCoreWorklist *);
void hirsel_core_counted_init(void *node, bool atomic);
void hirsel_core_share(void *node, bool atomic);
bool hirsel_core_try_unwrap(void *node, bool atomic);
void hirsel_core_release(HirselCoreContext *, void *node, bool atomic,
    uint64_t charge, HirselCoreDropRecipe);
void hirsel_core_release_enqueue(HirselCoreWorklist *, void *node, bool atomic,
    uint64_t charge, HirselCoreDropRecipe);
void hirsel_core_release_own_enqueue(HirselCoreWorklist *, void *block,
    uint64_t charge, HirselCoreDropRecipe);
HirselCoreAllocation hirsel_core_task_start(HirselCoreContext *);
HirselCoreAllocation hirsel_core_resource_alloc(const HirselCoreContext *,
    size_t bytes, size_t alignment, uint64_t logical_field_storage, uint64_t grant);
void hirsel_core_finalizer_begin(HirselCoreContext *, void *resource);
void hirsel_core_finalizer_end(HirselCoreContext *, void *resource);
void hirsel_core_resource_free(void *resource, uint64_t logical_field_storage);
/* Cause tags: 0 Overflow, 1 Bounds, 2 Division, 3 Unwrap, 4 Fail,
   5 Cancelled, 6 Capacity. Message ownership transfers to raise. */
typedef struct {
    uint8_t kind;
    uint64_t site;
    void *message;
    void (*drop_message)(void *);
} HirselCoreCause;
typedef struct HirselCoreCauseCell HirselCoreCauseCell;
void hirsel_core_raise(HirselCoreContext *, HirselCoreCause);
HirselCoreCauseCell *hirsel_core_causes_take(HirselCoreContext *);
void hirsel_core_causes_drop(HirselCoreCauseCell *);
const HirselCoreCauseCell *hirsel_core_cause_next(const HirselCoreCauseCell *);
const HirselCoreCause *hirsel_core_cause_value(const HirselCoreCauseCell *);
uint8_t hirsel_core_allocation_status(const HirselCoreContext *, void *allocation);
#endif
