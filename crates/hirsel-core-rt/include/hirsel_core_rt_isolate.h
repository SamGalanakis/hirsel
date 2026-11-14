#ifndef HIRSEL_CORE_RT_ISOLATE_H
#define HIRSEL_CORE_RT_ISOLATE_H
#include "hirsel_core_rt.h"
typedef struct { bool entered; uint64_t available; } HirselCoreIsolateEnter;
typedef struct { HirselCoreCauseCell *causes; uint8_t status; } HirselCoreIsolateCatch;
/* FORM #101 / FAIL-16. Reservation is a 32-byte cause cell. Refusal is a
   value, and returns the room without changing the task or cause chain. */
HirselCoreIsolateEnter hirsel_core_isolate_enter(HirselCoreContext *);
void hirsel_core_isolate_leave(HirselCoreContext *);
/* status 1 passes a Cancelled primary through with the saved chain restored;
   otherwise causes owns the caught chain and status is zero. */
HirselCoreIsolateCatch hirsel_core_isolate_catch(HirselCoreContext *);
#endif
