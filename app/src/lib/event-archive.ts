// Optimistic archive/unarchive for the typed event queue (archive contract v1),
// the archive twin of lib/event-decide.ts. Archiving a finished event posts
// `event_action{action:"archive",data:{}}` to the host IMMEDIATELY and sweeps
// the card out of the resting queue at once via the optimistic
// `event_archive_local` override (reconciled by the host's archived
// event_upsert — both actions are idempotent on the host). Archiving is
// reversible two ways, so there is no confirmation dialog and nothing red: a
// quiet "Archived" toast offers the immediate Undo, and the Archived(n) view's
// Unarchive is the durable path.

import { dispatch } from "../store/store";
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
 * a quiet "Archived" toast whose Undo unarchives. Returns the payload sent, so
 * callers (and tests) can assert or surface it. */
export function archiveEventWithUndo(
  eventId: number,
  opts?: { silent?: boolean },
): ArchivePayload {
  dispatch({ type: "event_archive_local", eventId });
  getClient()?.sendEventAction(eventId, "archive", {});
  if (!opts?.silent) {
    const toastId = toast("Archived", {
      action: {
        label: "Undo",
        onClick: () => {
          dismissToast(toastId);
          unarchiveEvent(eventId);
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
