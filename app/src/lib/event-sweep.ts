// The "Clear finished (n)" sweep (Wave-3) — a batch archive of every finished
// event in one wire op. Unlike per-card Archive (N `event_action{archive}`
// frames), the sweep sends ONE `clear_finished_events` frame and optimistically
// archives the whole batch locally (the `event_archive_local` flip per id, the
// same override archive uses), so the cards animate out with the shared archive
// exit and the counts drop in the same beat. The host archives the same finished
// set and echoes an `archived` `event_upsert` per event, reconciling the batch.
//
// Sweep is a QUIET verb (DESIGN.md): nothing red, no confirmation dialog — a
// single "Cleared n · Undo" toast owns recovery, and its Undo unarchives that
// exact batch (per-event `unarchive`, idempotent host-side).

import { dispatch } from "../store/store";
import { getClient } from "../ws/client";
import { toast } from "./toast";
import { unarchiveEvent } from "./event-archive";

/** Clear the given finished event ids in one sweep: optimistically archive each,
 * send the single `clear_finished_events` frame, and raise a "Cleared n · Undo"
 * toast whose Undo unarchives exactly that batch. No-op on an empty batch.
 * Returns the ids cleared so callers (and tests) can assert the batch. */
export function clearFinishedEventsWithUndo(finishedIds: number[]): number[] {
  const ids = [...finishedIds];
  if (ids.length === 0) return ids;
  for (const id of ids) dispatch({ type: "event_archive_local", eventId: id });
  getClient()?.clearFinishedEvents();
  toast(`Cleared ${ids.length}`, {
    action: {
      label: "Undo",
      // Bring the whole batch back — per-event unarchive is idempotent host-side,
      // so re-running it for a batch is safe.
      onClick: () => {
        for (const id of ids) unarchiveEvent(id);
      },
    },
  });
  return ids;
}
