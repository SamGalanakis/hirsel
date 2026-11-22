#ifndef HIRSEL_CORE_RT_H
#define HIRSEL_CORE_RT_H
#include <stdint.h>
#include <stddef.h>
/* Diagnostic archive only. Samples do not admit, deliver refunds or emit events.
 * Peak is the last run's root reservation, including bound/task/grant reservations. */
void hirsel_core_observe_charge(void);
uint64_t hirsel_core_peak_charge(void);
/* Observed archive only. Report bytes live until an observed run next starts
   or completes on this worker; len is a writable out count. No status/admission/event. */
const uint8_t *hirsel_core_memory_report(size_t *len);
/* Native harness phase, UTF-8 label; call between operations, with no ABI region
   mutation in progress. No source operation/status/admission/refund/event. */
void hirsel_core_memory_checkpoint(const uint8_t *name, size_t len);
#include <stdbool.h>
/* Frozen 64-bit emitter layouts; offsets in bytes, no atomic local accounting. */
#define HIRSEL_CORE_CONTEXT_CURRENT_LIMIT 0
#define HIRSEL_CORE_CONTEXT_EVENTS 8
#define HIRSEL_CORE_CONTEXT_STEPS 16
#define HIRSEL_CORE_LIMIT_AVAILABLE 0
#define HIRSEL_CORE_LIMIT_OUTSTANDING 8
#define HIRSEL_CORE_REGION_CURSOR 0
#define HIRSEL_CORE_REGION_END 8
#define HIRSEL_CORE_REGION_ENTRY 16
#define HIRSEL_CORE_REGION_PAYER 24
#define HIRSEL_CORE_REGION_CHARGE 32
#define HIRSEL_CORE_REGION_SIZE 40
#define HIRSEL_CORE_REGION_ALIGNMENT 48
#define HIRSEL_CORE_REGION_OCCUPIED 56
#define HIRSEL_CORE_REGION_STRIDE 64
#define HIRSEL_CORE_NODE_COUNT 0
#define HIRSEL_CORE_NODE_PAYLOAD 8
#define HIRSEL_CORE_GRAPH_PREFIX_BYTES 40
#define HIRSEL_CORE_RESOURCE_PREFIX_BYTES 8
#define HIRSEL_CORE_FIXED_DESCRIPTOR_RECIPE 0
#define HIRSEL_CORE_FIXED_DESCRIPTOR_CHARGE 8
#define HIRSEL_CORE_FIXED_DESCRIPTOR_PAYLOAD_OFFSET 16
#define HIRSEL_CORE_FIXED_DESCRIPTOR_BYTES 24
#define HIRSEL_CORE_FIXED_POINT_BYTES 40
#define HIRSEL_CORE_FIXED_POINT_NO_INDEX UINT64_MAX
#define HIRSEL_CORE_CAUSE_BYTES 32
#define HIRSEL_CORE_CAUSE_MESSAGE 0
#define HIRSEL_CORE_CAUSE_MESSAGE_DATA 0
#define HIRSEL_CORE_CAUSE_MESSAGE_LENGTH 8
#define HIRSEL_CORE_CAUSE_MESSAGE_CAPACITY 16
#define HIRSEL_CORE_CAUSE_KIND HIRSEL_CORE_CAUSE_MESSAGE_CAPACITY
#define HIRSEL_CORE_CAUSE_KIND_START UINT64_C(0x8000000000000000)
#define HIRSEL_CORE_CAUSE_FAIL 4
#define HIRSEL_CORE_CAUSE_SITE 24
#define HIRSEL_CORE_CAUSE_CELL_VALUE 0
#define HIRSEL_CORE_CAUSE_CELL_NEXT 32
#define HIRSEL_CORE_NATIVE_PLAIN 0
#define HIRSEL_CORE_NATIVE_GRAPH 1
#define HIRSEL_CORE_NATIVE_RESOURCE 2
typedef struct HirselCoreContext HirselCoreContext;
/* MEM-30. StepEnter false leaves the current bound unchanged. StepCatch closes
   its bound and refunds/catches only a chain of that bound's OutOfTime causes. */
bool hirsel_core_step_enter(HirselCoreContext *, uint64_t steps);
void hirsel_core_step_leave(HirselCoreContext *);
bool hirsel_core_step_catch(HirselCoreContext *);
uint8_t hirsel_core_step_out(HirselCoreContext *, uint64_t site);
typedef struct HirselCoreLimitRecord HirselCoreLimitRecord;
typedef struct { uint64_t requested, available; } HirselCoreCapacity;
typedef struct { void *pointer; HirselCoreCapacity capacity; } HirselCoreAllocation;
typedef struct { void *pointer; uint64_t available; } HirselCoreGrowth;
/* Consuming sequence growth; stride is the element's native size and the
 * shared reuse threshold is tested here. Success installs data/capacity before
 * refund and returns pointer/0. Capacity returns NULL/room without mutation.
 * The length word is unused and may be uninitialized. HostMemory is stop102
 * with no cleanup. kind is PLAIN/GRAPH, capacity was nonzero. */
HirselCoreGrowth hirsel_core_grow_cold(const HirselCoreContext *, void *carrier,
    size_t bytes, size_t align, uint64_t stride, uint64_t charge, uint64_t capacity,
    size_t moved, uint64_t refund, bool overflow, uint8_t kind);
/* rt.dest_id: no hidden context or failure status; never admits or allocates. */
uint64_t hirsel_core_dest_id(void);
/* Map hashing (FORM "Maps"). The caller provides five u64 words of scratch. */
uint64_t hirsel_core_map_hash_bytes(const HirselCoreContext *, const uint8_t *, uint64_t);
uint64_t hirsel_core_map_hash_word(const HirselCoreContext *, uint64_t);
void hirsel_core_map_hash_start(const HirselCoreContext *, void *);
void hirsel_core_map_hash_add_word(void *, uint64_t);
void hirsel_core_map_hash_add_bytes(void *, const uint8_t *, uint64_t);
uint64_t hirsel_core_map_hash_finish(const void *);
/* rt.sha1_blocks: RFC 3174 compression of the byte span and its padding. */
void hirsel_core_sha1_blocks(uint32_t *state, const uint8_t *data, uint64_t len);
/* rt.zero_bytes: zero len bytes at at, inside the storage block that starts at
   block; pages the kernel never backed are already zero and stay untouched. */
void hirsel_core_zero_bytes(uint8_t *block, uint8_t *at, uint64_t len);

/* rt.sha256_blocks: FIPS 180-4 compression of blocks 64-byte blocks into the
   eight state words, with the CPU's SHA-256 instructions when it has them; no
   context or status; zero blocks read nothing. */
void hirsel_core_sha256_blocks(uint32_t *state, const uint8_t *data, uint64_t blocks);

/* Signature verification. Returns 1 for a valid signature, 0 otherwise.
   Empty ranges are never read. No context or failure status. */
uint64_t hirsel_core_ed25519_verify(const uint8_t *key, uint64_t key_len,
                                   const uint8_t *msg, uint64_t msg_len,
                                   const uint8_t *sig, uint64_t sig_len);
uint64_t hirsel_core_p256_verify(const uint8_t *key, uint64_t key_len,
                                const uint8_t *msg, uint64_t msg_len,
                                const uint8_t *sig, uint64_t sig_len);
uint64_t hirsel_core_p256_verify_der(const uint8_t *key, uint64_t key_len,
                                    const uint8_t *msg, uint64_t msg_len,
                                    const uint8_t *sig, uint64_t sig_len);

/* XXH3-64 seed zero for a span of at most 240 bytes. */
uint64_t hirsel_core_xxh3_short(const uint8_t *data, uint64_t len);

/* Add count 64-byte stripes with secret offsets advancing eight bytes. */
void hirsel_core_xxh3_stripes(uint64_t *acc, const uint8_t *data, const uint8_t *secret, uint64_t count);
/* rt.compare_bytes: bytewise order (SEM-7), -1, 0 or 1, a proper prefix first; no
   context or failure status; an empty range is never read. */
int8_t hirsel_core_compare_bytes(const uint8_t *a, uint64_t a_len, const uint8_t *b,
                                 uint64_t b_len);
HirselCoreContext *hirsel_core_context_new(uint64_t bytes);
void hirsel_core_context_free(HirselCoreContext *);
/* A compiler-generated flow call. The four result words are command-specific. */
uint8_t hirsel_core_flow(HirselCoreContext *, void *, uint8_t,
    const uint64_t *, uint64_t (*)[4], uint64_t site);
void hirsel_core_flow_drop(void *);
HirselCoreAllocation hirsel_core_alloc(const HirselCoreContext *, void *reserved,
    size_t bytes, size_t alignment, uint64_t charge, bool charge_overflow, uint8_t native_kind);
void hirsel_core_free(void *, uint64_t remaining_charge);
HirselCoreAllocation hirsel_core_limit_enter(HirselCoreContext *, uint64_t bytes);
void hirsel_core_limit_exit(HirselCoreContext *);
typedef struct HirselCoreRegion HirselCoreRegion;
HirselCoreRegion *hirsel_core_region_open(const HirselCoreContext *);
HirselCoreAllocation hirsel_core_region_alloc(const HirselCoreContext *,
    HirselCoreRegion *, size_t bytes, size_t alignment, uint64_t charge, bool charge_overflow);
void hirsel_core_region_lower_charge(HirselCoreRegion *, size_t entry,
    uint64_t previous_charge, uint64_t retained_charge);
void hirsel_core_region_close(HirselCoreRegion *);
typedef struct HirselCoreWorklist HirselCoreWorklist;
/* Resumable glue: cursor starts at 0. Advance it, enqueue at most one child,
   return false; return true only after all children and fields are cleaned. */
typedef bool (*HirselCoreDropRecipe)(HirselCoreContext *, void *payload, uint64_t *cursor, HirselCoreWorklist *);
/* Shared immutable Fixed metadata, not a language allocation, followed inline by
   its suspension points; point k (1-based) at descriptor + 24 + 40(k-1), with
   back = that distance. A suspended block holds the point address at payload+a,
   the next link at payload+b and, inside an inline array, the cursor at
   payload+index. No status or admission for the entries below; enqueue hands
   one descendant to the drain from the running parent's point (0 under a
   buffer), last drains without recursion. Descriptor survives the compiled run. */
typedef struct { HirselCoreDropRecipe recipe; uint64_t charge, payload_offset; } HirselCoreFixedDescriptor;
typedef struct { uint64_t back, cursor, a, b, index; } HirselCoreFixedPoint;
bool hirsel_core_release(HirselCoreContext *, void *, const HirselCoreFixedDescriptor *, uint64_t site);
/* A dead box compiled glue cleaned in place, freed as the drain frees it. */
void hirsel_core_free_box(HirselCoreContext *, void *node, uint64_t charge);
/* Inline Arc completion hooks. Call with ctx[CONTEXT_EVENTS] != 0.
   The event word and sink are pinned for the run. The hook folds owner
   credits and records the operation. Release precedes last-release glue. */
void hirsel_core_arc_initialized(const HirselCoreContext *);
void hirsel_core_arc_shared(const HirselCoreContext *, const void *node); /* loads completed new count */
void hirsel_core_arc_released(const HirselCoreContext *, bool last);
void hirsel_core_arc_unwrapped(const HirselCoreContext *, bool unique);
void hirsel_core_counted_init(void *node, bool atomic);
void hirsel_core_share(void *node, bool atomic);
_Noreturn void hirsel_core_count_overflow(void);
bool hirsel_core_try_unwrap(void *node, bool atomic);
bool hirsel_core_arc_decrement_for_replay(void *node);
HirselCoreAllocation hirsel_core_task_start(HirselCoreContext *);
HirselCoreAllocation hirsel_core_resource_alloc(const HirselCoreContext *,
    size_t bytes, size_t alignment, uint64_t logical_field_storage, uint64_t grant);
void hirsel_core_finalizer_begin(HirselCoreContext *, void *resource);
void hirsel_core_finalizer_end(HirselCoreContext *, void *resource);
void hirsel_core_resource_free(void *resource, uint64_t logical_field_storage);
/* Cause kinds: 0 Overflow, 1 Bounds, 2 Division, 3 Unwrap, 4 Fail,
   5 Cancelled, 6 Capacity, 7 OutOfTime. The kind is Fail's message's niche (FORM #75): kind k
   other than Fail is message.capacity == HIRSEL_CORE_CAUSE_KIND_START + k; a Fail's
   capacity is below that. Message ownership transfers to raise. */
typedef struct { uint8_t *data; uint64_t len, capacity; } HirselCoreStringMessage;
typedef struct {
    HirselCoreStringMessage message;
    uint64_t site;
} HirselCoreCause;
static inline uint8_t hirsel_core_cause_kind(const HirselCoreCause *cause) {
    uint64_t word = cause->message.capacity;
    return word < HIRSEL_CORE_CAUSE_KIND_START ? HIRSEL_CORE_CAUSE_FAIL
                                               : (uint8_t)(word - HIRSEL_CORE_CAUSE_KIND_START);
}
/* A cause of a kind other than Fail. */
static inline HirselCoreCause hirsel_core_cause_new(uint8_t kind, uint64_t site) {
    HirselCoreCause cause = {0};
    cause.message.capacity = HIRSEL_CORE_CAUSE_KIND_START + kind;
    cause.site = site;
    return cause;
}
typedef struct HirselCoreCauseCell HirselCoreCauseCell;
/* Transfers the inline String backing; capacity zero owns no storage. */
void hirsel_core_raise(HirselCoreContext *, HirselCoreCause);
void hirsel_core_raise_builtin(HirselCoreContext *, uint8_t kind, uint64_t site);
void hirsel_core_raise_string(HirselCoreContext *, uint64_t site,
    uint8_t *data, uint64_t len, uint64_t capacity);
/* Capacity(c) for a refused admission: requested and available at 0 and 8. */
void hirsel_core_raise_capacity(HirselCoreContext *, uint64_t site,
    uint64_t requested, uint64_t available);
HirselCoreCauseCell *hirsel_core_causes_take(HirselCoreContext *);
void hirsel_core_causes_drop(HirselCoreCauseCell *);
const HirselCoreCauseCell *hirsel_core_cause_next(const HirselCoreCauseCell *);
const HirselCoreCause *hirsel_core_cause_value(const HirselCoreCauseCell *);
/* All return unit except region_refill (nonnull payload); no failure statuses. */
void hirsel_core_refunds_drain(const HirselCoreContext *);
/* Bind on resume, null to unbind before transferring execution custody. */
void hirsel_core_task_bind(const HirselCoreContext *);
void *hirsel_core_region_refill(HirselCoreRegion *, size_t bytes, size_t alignment);
void hirsel_core_refund(HirselCoreLimitRecord *original_payer, uint64_t nonzero_bytes);
/* Context-aware refund: compare raw payer word with current, direct when equal;
   otherwise use original-payer lookup. No status/admission/allocation. */
void hirsel_core_refund_in(const HirselCoreContext *, HirselCoreLimitRecord *original_payer, uint64_t bytes);
/* Admission without allocation, for storage the caller holds (frame bytes). */
HirselCoreAllocation hirsel_core_admit(const HirselCoreContext *, uint64_t charge, bool charge_overflow);
HirselCoreAllocation hirsel_core_admit_lent_frame(const HirselCoreContext *, uint8_t *frame, uint64_t charge, bool charge_overflow);
uint8_t *hirsel_core_promote_frame_list(const HirselCoreContext *, uint8_t *data, uint64_t capacity, uint64_t len, size_t stride);
/* Physical backing for a block already admitted by the current task. */
void *hirsel_core_alloc_admitted(const HirselCoreContext *, HirselCoreLimitRecord *, size_t bytes,
    size_t alignment, uint8_t kind);
HirselCoreLimitRecord *hirsel_core_block_payer(void *);
void hirsel_core_free_value(const HirselCoreContext *, void *, uint64_t);
/* Recipe borrows its descriptor from the still-live parent frame. */
bool hirsel_core_release_buffer(HirselCoreContext *, void *buffer, uint64_t charge, void *carrier, HirselCoreDropRecipe, uint64_t site);
bool hirsel_core_resource_release(HirselCoreContext *, void *resource, HirselCoreDropRecipe, uint64_t storage, uint64_t site);
void hirsel_core_resource_stage(void *resource, HirselCoreDropRecipe, uint64_t storage, HirselCoreWorklist *, uint64_t point);
void hirsel_core_release_buffer_enqueue(HirselCoreWorklist *, void *buffer, uint64_t charge, void *payload, HirselCoreDropRecipe, uint64_t point);
#include "hirsel_core_rt_host.h"
#include "hirsel_core_rt_tasks.h"
#include "hirsel_core_rt_isolate.h"
#endif
