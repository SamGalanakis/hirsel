// Optimistic archive/unarchive for Tasks (typed Event wire contract),
// the archive twin of lib/event-decide.ts. Archiving ANY non-archived event
// (finished or still-open) posts `event_action{action:"archive",data:{}}` to the
// host IMMEDIATELY and removes the Task from the active field at once via the
// optimistic `event_archive_local` override (reconciled by the host's archived
// event_upsert — both actions are idempotent on the host). Archiving an open
// event auto-resolves it host-side (`archived=1, status='done'`). Archiving is
// reversible two ways, so there is no confirmation dialog and nothing red: a
// quiet "Archived" toast offers the immediate Undo, and the Archived(n) view's
// Unarchive is the durable path.

import { reopenEvent } from "./event-decide";
import { isEventResolved } from "../store/selectors";
import { dispatch, effectiveEvents } from "../store/store";
import { getClient } from "../ws/client";
import { dismissToast, toast } from "./toast";

/** The exact wire envelope (archive contract v1): `data` is the empty object,
 * not null — `{"type":"event_action","event_id":N,"action":"archive","data":{}}`. */
export interface ArchivePayload {
  type: "event_action";
  event_id: number;
  action: "archive" | "unarchive";
  data: Record<string, never>;
}

export function archivePayload(eventId: number, action: "archive" | "unarchive"): ArchivePayload {
  return { type: "event_action", event_id: eventId, action, data: {} };
}

/** Archive an event: optimistic sweep + immediate `event_action{archive}`, with
 * a quiet "Archived" toast whose Undo restores the event honestly. Returns the
 * payload sent, so callers (and tests) can assert or surface it.
 *
 * Honest Undo (archive contract): archiving an event that is STILL OPEN
 * auto-dismisses it host-side (`archived=1, status='done'`), and unarchiving does
 * NOT reopen what the archive auto-dismissed. So the Undo for a card that was
 * open at archive time must restore it fully — unarchive AND reopen (drop the
 * decide override too, which `reopenEvent` already does). A finished card (decided
 * or read awareness) was never auto-dismissed, so its Undo unarchives only. We
 * read the open/finished state from the store at call time (before the optimistic
 * sweep, which never touches status or the decide overrides) so callers stay
 * unchanged. Both host ops are idempotent. */
export function archiveEventWithUndo(
  eventId: number,
  opts?: { silent?: boolean },
): ArchivePayload {
  // Read the PROJECTED event before dispatching: the optimistic sweep below
  // asserts `archived`, never `status`, but reading first keeps the open/finished
  // question answered against the state the Owner actually saw.
  const event = effectiveEvents().find((e) => e.id === eventId);
  const wasOpen = event ? !isEventResolved(event) : false;
  dispatch({ type: "event_archive_local", eventId });
  getClient()?.sendEventAction(eventId, "archive", {});
  if (!opts?.silent) {
    const toastId = toast("Archived", {
      action: {
        label: "Undo",
        onClick: () => {
          dismissToast(toastId);
          unarchiveEvent(eventId);
          if (wasOpen) reopenEvent(eventId);
        },
      },
    });
  }
  return archivePayload(eventId, "archive");
}

/** Unarchive (the Archived view's row action, or the toast Undo): optimistic
 * return to the resting queue + `event_action{unarchive}`. Per the contract,
 * unarchiving never reopens a judgment the archive auto-dismissed — the row
 * simply returns as the decided/read event it is. */
export function unarchiveEvent(eventId: number): ArchivePayload {
  dispatch({ type: "event_unarchive_local", eventId });
  getClient()?.sendEventAction(eventId, "unarchive", {});
  return archivePayload(eventId, "unarchive");
}
