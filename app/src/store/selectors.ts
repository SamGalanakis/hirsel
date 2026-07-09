import type { InboxItem } from "../protocol";
import type { DisplayMessage } from "./types";

/** Effective "seen" state for an Inbox item (v1.3). An item is read when the
 * wire `read` flag is true AND the Owner has not manually "Marked unread" it
 * (a client-only override). Kept in one place so the badge, the card visual
 * state, and the auto-read gate all agree by construction. */
export function isItemRead(item: InboxItem, unreadOverrides: number[]): boolean {
  return item.read === true && !unreadOverrides.includes(item.id);
}

/** Count backing both the Inbox tab badge and the document.title badge. v1.3:
 * email-like "unread" count = open items that are not yet effectively read
 * (was open + requires_response). requires_response no longer affects the
 * badge — it only drives the card accent. */
export function openUnreadCount(inbox: InboxItem[], unreadOverrides: number[]): number {
  return inbox.filter((i) => i.status === "open" && !isItemRead(i, unreadOverrides)).length;
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
