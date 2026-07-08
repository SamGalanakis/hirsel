import { useEffect } from "react";
import { useStore } from "../store/store";
import { openRequiresResponseCount } from "../store/selectors";

const BASE_TITLE = "hirsel";

/** Reflects the Inbox's requires-response badge count in document.title, so
 * it's visible from a backgrounded tab without needing push notifications
 * (deferred, see docs/SCOPE.md). */
export function useTitleBadge(): number {
  const count = useStore((s) => openRequiresResponseCount(s.inbox));

  useEffect(() => {
    document.title = count > 0 ? `(${count}) ${BASE_TITLE}` : BASE_TITLE;
  }, [count]);

  return count;
}
