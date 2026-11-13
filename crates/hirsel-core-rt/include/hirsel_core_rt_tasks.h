#ifndef HIRSEL_CORE_TASKS_H
#define HIRSEL_CORE_TASKS_H
#include "hirsel_core_rt.h"
typedef uint8_t (*HirselCoreValueDrop)(HirselCoreContext *, void *);
typedef struct {
    size_t bytes, alignment;
    uint64_t logical;
    HirselCoreValueDrop drop;
} HirselCoreValueLayout;
typedef struct { HirselCoreValueLayout input, result; } HirselCoreTaskLayout;
typedef uint8_t (*HirselCoreTaskBody)(HirselCoreContext *, void *destination, void *input);
typedef struct HirselCoreTask HirselCoreTask;
typedef struct { HirselCoreTask *task; uint64_t available; } HirselCoreSpawn;
/* Emitters specialize FORM rows with closed layout/drop constants, then call
   these generic entries. Input remains caller-owned on null spawn. */
HirselCoreSpawn hirsel_core_spawn(HirselCoreContext *, uint64_t bytes, void *input,
    HirselCoreTaskBody, HirselCoreTaskLayout);
void hirsel_core_cancel(HirselCoreContext *, HirselCoreTask *);
typedef struct { uint8_t kind; HirselCoreCauseCell *causes; uint8_t status; } HirselCoreJoin;
/* kind: 0 result, 1 Failed(chain), 2 Cancelled(chain).
   status 1 means the joiner failed, still owns the handle for cleanup. */
HirselCoreJoin hirsel_core_join(HirselCoreContext *, HirselCoreTask *, void *result, uint64_t site);
/* Result finalizer failure belongs to the abandoning task: returns status 1. */
uint8_t hirsel_core_task_drop(HirselCoreContext *, HirselCoreTask *);
typedef struct HirselCoreChannel HirselCoreChannel;
typedef struct { HirselCoreChannel *sender, *receiver; uint64_t available; uint8_t status; } HirselCoreChannelNew;
HirselCoreChannelNew hirsel_core_channel_new(HirselCoreContext *, uint64_t capacity, HirselCoreValueLayout, uint64_t site);
HirselCoreChannel *hirsel_core_channel_sender(HirselCoreChannel *);
typedef struct { bool transferred; uint8_t status; } HirselCoreTransfer;
HirselCoreTransfer hirsel_core_send(HirselCoreContext *, HirselCoreChannel *, void *input, uint64_t site);
uint8_t hirsel_core_try_send(HirselCoreChannel *, void *input);
HirselCoreTransfer hirsel_core_receive(HirselCoreContext *, HirselCoreChannel *, void *destination, uint64_t site);
void hirsel_core_sender_drop(HirselCoreChannel *);
uint8_t hirsel_core_receiver_drop(HirselCoreContext *, HirselCoreChannel *);
/* Event dispatch after an emitted Arc update/test. These do not change counts,
   admit storage or return a failure status. release_last follows the last event. */
void hirsel_core_arc_shared(const void *node);
void hirsel_core_arc_released(bool last);
void hirsel_core_arc_unwrapped(bool unique);
#endif
