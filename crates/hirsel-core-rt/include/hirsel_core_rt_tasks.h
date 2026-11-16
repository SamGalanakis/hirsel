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
/* Full is a no-effect poll, not a cancellation point. Poll fairness offers
   other runnable tasks a turn at least every 64 polls; Ok never switches.
   The decision is fixed before scheduling, including if a peer drains it. */
uint8_t hirsel_core_try_send(HirselCoreChannel *, void *input);
HirselCoreTransfer hirsel_core_receive(HirselCoreContext *, HirselCoreChannel *, void *destination, uint64_t site);
/* Empty is a poll (scheduler choice, no cancellation); Closed is terminal. */
uint8_t hirsel_core_try_receive(HirselCoreChannel *, void *destination);
void hirsel_core_sender_drop(HirselCoreChannel *);
uint8_t hirsel_core_receiver_drop(HirselCoreContext *, HirselCoreChannel *);
/* CONC-13 Latest[T]: one swappable Arc holder. new admits the root (null keeps
   node with the caller); read returns the root's node for the caller to share;
   publish installs node and returns the old holder for the caller to release.
   Events: SENDERS for the handle count, LATEST_PUBLISH, LATEST_READ
   (value 0). None waits, fails or observes cancellation. */
typedef struct HirselCoreLatest HirselCoreLatest;
typedef struct { HirselCoreLatest *latest; uint64_t available; } HirselCoreLatestNew;
HirselCoreLatestNew hirsel_core_latest_new(HirselCoreContext *, void *node, HirselCoreValueDrop);
HirselCoreLatest *hirsel_core_latest_handle(HirselCoreLatest *);
void *hirsel_core_latest_publish(HirselCoreLatest *, void *node);
void *hirsel_core_latest_read(HirselCoreLatest *);
void hirsel_core_latest_drop(HirselCoreContext *, HirselCoreLatest *);
/* Scheduler events include CHARGE (effectful foreign growth), CHECK_CANCEL
   (no request) and CAPACITY (requested, available). Only Full, Empty, no-request
   check, failed Arc unwrap and Capacity count as polls. */
/* Event dispatch after an emitted Arc update/test. These do not change counts,
   admit storage or return a failure status. release_last follows the last event. */
#endif
