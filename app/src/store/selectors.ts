import type { InboxItem } from "../protocol";

/** Count backing both the Inbox tab badge and the document.title badge
 * (spec: "Tab badge = count of open requires_response items; also reflect it
 * in document.title"). Kept in one place so both stay in sync by construction. */
export function openRequiresResponseCount(inbox: InboxItem[]): number {
  return inbox.filter((i) => i.status === "open" && i.requires_response).length;
}
