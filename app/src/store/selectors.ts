import type { InboxItem } from "../protocol";
import type { DisplayMessage } from "./types";

/** Count backing both the Inbox tab badge and the document.title badge
 * (spec: "Tab badge = count of open requires_response items; also reflect it
 * in document.title"). Kept in one place so both stay in sync by construction. */
export function openRequiresResponseCount(inbox: InboxItem[]): number {
  return inbox.filter((i) => i.status === "open" && i.requires_response).length;
}

/** The Owner's reply to an Inbox Item, derived — never persisted — from Chat.
 * A reply is just an anchor-refed owner Chat message (`ref === item.anchor`),
 * so its lifecycle (optimistic `pending` → echoed `✓`, or `failed`) is exactly
 * the send it already is. */
export interface ItemReply {
  body: string;
  pending: boolean;
  failed: boolean;
}

/** Latest Owner reply anchored to `anchor`, or null if none yet. Drives an
 * Inbox card's inline "you: … ✓" replied state. Derived from Chat messages so
 * there is no new inbox reply state to persist or reconcile; when the send
 * reconciles from optimistic to host echo, `pending` flips to false by itself.
 * Newest-first scan: if the Owner answers the same item twice, the most recent
 * reply wins. */
export function latestReplyForAnchor(
  messages: DisplayMessage[],
  anchor: number,
): ItemReply | null {
  for (let i = messages.length - 1; i >= 0; i--) {
    const m = messages[i];
    if (m.author === "owner" && m.ref === anchor) {
      return { body: m.body, pending: m.pending === true, failed: m.failed === true };
    }
  }
  return null;
}
