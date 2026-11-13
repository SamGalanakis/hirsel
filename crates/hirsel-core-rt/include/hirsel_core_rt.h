#ifndef HIRSEL_CORE_RT_H
#define HIRSEL_CORE_RT_H
#include <stdint.h>
#include <stddef.h>
#include <stdbool.h>
/* Frozen 64-bit emitter layouts; offsets in bytes, no atomic local accounting. */
#define HIRSEL_CORE_CONTEXT_CURRENT_LIMIT 0
#define HIRSEL_CORE_CONTEXT_EVENTS 8
#define HIRSEL_CORE_LIMIT_AVAILABLE 0
#define HIRSEL_CORE_LIMIT_OUTSTANDING 8
#define HIRSEL_CORE_REGION_CURSOR 0
#define HIRSEL_CORE_REGION_END 8
#define HIRSEL_CORE_REGION_ENTRY 16
#define HIRSEL_CORE_REGION_PAYER 24
#define HIRSEL_CORE_REGION_CHARGE 32
#define HIRSEL_CORE_NODE_COUNT 0
#define HIRSEL_CORE_NODE_PAYLOAD 8
#define HIRSEL_CORE_OWN_PAYER_BACK 8
#define HIRSEL_CORE_OWN_TAG_MASK 31
#define HIRSEL_CORE_OWN_KIND_MASK 3
#define HIRSEL_CORE_OWN_ALIGNMENT_SHIFT 2
#define HIRSEL_CORE_OWN_ORDINARY_PLAIN_TAG 0
#define HIRSEL_CORE_OWN_ORDINARY_GRAPH_TAG 1
#define HIRSEL_CORE_OWN_ORDINARY_MAX_TAG HIRSEL_CORE_OWN_ORDINARY_GRAPH_TAG
#define HIRSEL_CORE_PLAIN_PREFIX_BYTES 8
#define HIRSEL_CORE_GRAPH_PREFIX_BYTES 48
#define HIRSEL_CORE_RESOURCE_PREFIX_BYTES 16
#define HIRSEL_CORE_CAUSE_BYTES 40
#define HIRSEL_CORE_CAUSE_KIND 0
#define HIRSEL_CORE_CAUSE_MESSAGE 8
#define HIRSEL_CORE_CAUSE_MESSAGE_DATA 8
#define HIRSEL_CORE_CAUSE_MESSAGE_LENGTH 16
#define HIRSEL_CORE_CAUSE_MESSAGE_CAPACITY 24
#define HIRSEL_CORE_CAUSE_SITE 32
#define HIRSEL_CORE_CAUSE_CELL_VALUE 0
#define HIRSEL_CORE_CAUSE_CELL_NEXT 40
#define HIRSEL_CORE_NATIVE_PLAIN 0
#define HIRSEL_CORE_NATIVE_GRAPH 1
#define HIRSEL_CORE_NATIVE_RESOURCE 2
typedef struct HirselCoreContext HirselCoreContext;
typedef struct HirselCoreLimitRecord HirselCoreLimitRecord;
typedef struct { uint64_t requested, available; } HirselCoreCapacity;
typedef struct { void *pointer; HirselCoreCapacity capacity; } HirselCoreAllocation;
/* rt.dest_id: no hidden context or failure status; never admits or allocates. */
uint64_t hirsel_core_dest_id(void);
/* rt.hash_bytes: the fixed Map byte hash (FORM "Maps"); no context or status. */
uint64_t hirsel_core_hash_bytes(const uint8_t *data, uint64_t len);
/* rt.compare_bytes: bytewise order (SEM-7), -1, 0 or 1, a proper prefix first; no
   context or failure status; an empty range is never read. */
int8_t hirsel_core_compare_bytes(const uint8_t *a, uint64_t a_len, const uint8_t *b,
                                 uint64_t b_len);
HirselCoreContext *hirsel_core_context_new(uint64_t bytes);
void hirsel_core_context_free(HirselCoreContext *);
HirselCoreAllocation hirsel_core_alloc(const HirselCoreContext *, void *like,
    size_t bytes, size_t alignment, uint64_t charge, bool charge_overflow, uint8_t native_kind);
void hirsel_core_free(void *, uint64_t remaining_charge);
HirselCoreAllocation hirsel_core_limit_enter(HirselCoreContext *, uint64_t bytes);
void hirsel_core_limit_exit(HirselCoreContext *);
typedef struct HirselCoreRegion HirselCoreRegion;
HirselCoreRegion *hirsel_core_region_open(size_t physical_chunk_bytes);
HirselCoreAllocation hirsel_core_region_alloc(const HirselCoreContext *,
    HirselCoreRegion *, size_t bytes, size_t alignment, uint64_t charge, bool charge_overflow);
void hirsel_core_region_lower_charge(HirselCoreRegion *, size_t entry,
    uint64_t previous_charge, uint64_t retained_charge);
void hirsel_core_region_close(HirselCoreRegion *);
typedef struct HirselCoreWorklist HirselCoreWorklist;
/* Resumable glue: cursor starts at 0. Advance it, enqueue at most one child,
   return false; return true only after all children and fields are cleaned. */
typedef bool (*HirselCoreDropRecipe)(HirselCoreContext *, void *payload, uint64_t *cursor, HirselCoreWorklist *);
/* Inline Arc completion hooks: call only when ctx[CONTEXT_EVENTS] != 0.
   The event word/sink are immutable during a run. No TLS read on the taken path.
   No status/admission/suspension/count mutation.
   Scheduler owns the event turn across mutation and hook; release precedes glue. */
enum { HIRSEL_CORE_ARC_EVENT_NEW=10, HIRSEL_CORE_ARC_EVENT_SHARE=11,
       HIRSEL_CORE_ARC_EVENT_RELEASE=12, HIRSEL_CORE_ARC_EVENT_UNWRAP=13 };
void hirsel_core_arc_initialized(const HirselCoreContext *);
void hirsel_core_arc_shared(const HirselCoreContext *, const void *node); /* loads completed new count */
void hirsel_core_arc_released(const HirselCoreContext *, bool last);
void hirsel_core_arc_unwrapped(const HirselCoreContext *, bool unique);
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
typedef struct { uint8_t *data; uint64_t len, capacity; } HirselCoreStringMessage;
typedef struct {
    uint8_t kind;
    HirselCoreStringMessage message;
    uint64_t site;
} HirselCoreCause;
typedef struct HirselCoreCauseCell HirselCoreCauseCell;
/* Transfers the inline String backing; capacity zero owns no storage. */
void hirsel_core_raise(HirselCoreContext *, HirselCoreCause);
void hirsel_core_raise_string(HirselCoreContext *, uint64_t site,
    uint8_t *data, uint64_t len, uint64_t capacity);
HirselCoreCauseCell *hirsel_core_causes_take(HirselCoreContext *);
void hirsel_core_causes_drop(HirselCoreCauseCell *);
const HirselCoreCauseCell *hirsel_core_cause_next(const HirselCoreCauseCell *);
const HirselCoreCause *hirsel_core_cause_value(const HirselCoreCauseCell *);
/* All return unit except region_refill (nonnull payload); no failure statuses. */
void hirsel_core_refunds_drain(const HirselCoreContext *);
/* Bind on resume, null to unbind before transferring execution custody. */
void hirsel_core_task_bind(const HirselCoreContext *);
void *hirsel_core_region_refill(HirselCoreRegion *, size_t bytes, size_t alignment);
void hirsel_core_release_last(HirselCoreContext *, void *node, uint64_t charge, HirselCoreDropRecipe);
void hirsel_core_refund(HirselCoreLimitRecord *original_payer, uint64_t nonzero_bytes);
/* Context-aware refund: compare raw payer word with current, direct when equal;
   otherwise use original-payer lookup. No status/admission/allocation. */
void hirsel_core_refund_in(const HirselCoreContext *, HirselCoreLimitRecord *original_payer, uint64_t bytes);
void *hirsel_core_native_alloc(size_t native_bytes, size_t native_alignment);
void hirsel_core_native_free(void *base);
uint8_t hirsel_core_allocation_status(const HirselCoreContext *, void *);
void hirsel_core_free_value(void *, uint64_t);
/* Recipe borrows its descriptor from the still-live parent frame. */
void hirsel_core_release_buffer_enqueue(HirselCoreWorklist *, void *buffer, uint64_t charge, void *payload, HirselCoreDropRecipe);
#include "hirsel_core_rt_host.h"
#include "hirsel_core_rt_tasks.h"
#endif
