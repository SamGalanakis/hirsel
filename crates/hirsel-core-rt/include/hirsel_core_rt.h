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
#endif
