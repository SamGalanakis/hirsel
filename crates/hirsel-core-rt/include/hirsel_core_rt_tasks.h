#ifndef HIRSEL_CORE_TASKS_H
#define HIRSEL_CORE_TASKS_H
#include "hirsel_core_rt.h"
/* clean: the value's resumable cleanup in place, run on the active drain;
   null for a task input, which the child body always cleans. */
typedef struct {
    size_t bytes, alignment;
    uint64_t logical;
    HirselCoreDropRecipe clean;
} HirselCoreValueLayout;
typedef struct { HirselCoreValueLayout input, result; } HirselCoreTaskLayout;
typedef uint8_t (*HirselCoreTaskBody)(HirselCoreContext *, void *destination, void *input);
typedef struct HirselCoreTask HirselCoreTask;
typedef struct { HirselCoreTask *task; uint64_t available; uint8_t status; } HirselCoreSpawn;
/* FORM #121: spawn carries the Tasks grant and site and returns a failure
   status for a taped grant that has ended or faulted. Capacity remains a null
   task plus available, with status zero. Emitters specialize FORM rows with closed layout/drop constants, then call
   these generic entries. Input remains caller-owned on null spawn. */
HirselCoreSpawn hirsel_core_spawn(HirselCoreContext *, uint64_t tasks, uint64_t bytes, void *input,
    HirselCoreTaskBody, HirselCoreTaskLayout, uint64_t site);
void hirsel_core_cancel(HirselCoreContext *, HirselCoreTask *);
typedef struct { uint8_t kind; HirselCoreCauseCell *causes; uint8_t status; } HirselCoreJoin;
/* kind: 0 result, 1 Failed(chain), 2 Cancelled(chain).
   status 1 means the joiner failed, still owns the handle for cleanup. */
HirselCoreJoin hirsel_core_join(HirselCoreContext *, HirselCoreTask *, void *result, uint64_t site);
/* Result finalizer failure belongs to the abandoning task: returns status 1. */
uint8_t hirsel_core_task_drop(HirselCoreContext *, HirselCoreTask *);
/* Inside a running drain, the holder's cleanup is handed over at point:
   these return whether it was (receiver and task always; Latest at its last handle). */
bool hirsel_core_task_release(HirselCoreContext *, HirselCoreTask *, HirselCoreWorklist *, uint64_t point);
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
/* kind: 0 item, 1 Empty, 2 Closed. Status 1 is cancellation or clock failure. */
typedef struct { uint8_t kind, status; } HirselCoreReceiveUntil;
HirselCoreReceiveUntil hirsel_core_receive_until(HirselCoreContext *, HirselCoreChannel *,
    void *destination, uint8_t present, uint64_t clock, uint64_t deadline, uint64_t site);
/* Empty is a poll (scheduler choice, no cancellation); Closed is terminal. */
uint8_t hirsel_core_try_receive(HirselCoreChannel *, void *destination);
void hirsel_core_sender_drop(HirselCoreChannel *);
uint8_t hirsel_core_receiver_drop(HirselCoreContext *, HirselCoreChannel *);
bool hirsel_core_receiver_release(HirselCoreContext *, HirselCoreChannel *, HirselCoreWorklist *, uint64_t point);
/* CONC-13 Latest[T]: one swappable Arc holder. new admits the root (null keeps
   node with the caller); read returns a new share of the root's Arc.
   publish installs node and releases the old holder. The Arc event immediately
   follows the Latest event.
   Events: SENDERS for the handle count, LATEST_PUBLISH, LATEST_READ
   (value 0). None waits, fails or observes cancellation. */
typedef struct HirselCoreLatest HirselCoreLatest;
typedef struct { HirselCoreLatest *latest; uint64_t available; } HirselCoreLatestNew;
HirselCoreLatestNew hirsel_core_latest_new(HirselCoreContext *, void *node, HirselCoreDropRecipe);
HirselCoreLatest *hirsel_core_latest_handle(HirselCoreLatest *);
void hirsel_core_latest_publish(HirselCoreContext *, HirselCoreLatest *, void *node);
void *hirsel_core_latest_read(HirselCoreLatest *);
void hirsel_core_latest_drop(HirselCoreContext *, HirselCoreLatest *);
bool hirsel_core_latest_release(HirselCoreLatest *, HirselCoreWorklist *, uint64_t point);
/* Shared nodes (CONC-15): node points at the atomic count, the payload at +8.
   new admits charge and moves the cross value at x, described by cross
   (bytes, owner rows, row count), into a node homed in the running unit (null
   node: Capacity). share and release are not events; the last release cleans
   and refunds at home. */
typedef struct { void *node; uint64_t available; } HirselCoreSharedNew;
HirselCoreSharedNew hirsel_core_shared_new(HirselCoreContext *, const void *x, const void *cross, uint64_t charge);
void *hirsel_core_shared_share(void *node);
void hirsel_core_shared_release(HirselCoreContext *, void *node);
/* Scheduler events include REFUND (foreign refund or hand-over), CHECK_CANCEL
   (no request) and CAPACITY (requested, available). Only Full, Empty, no-request
   check, failed Arc unwrap and Capacity count as polls. */
/* Event dispatch after an emitted Arc update/test. These do not change counts,
   admit storage or return a failure status. release_last follows the last event. */
#endif
