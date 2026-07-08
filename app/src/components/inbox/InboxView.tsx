import { useMemo, useState } from "react";
import type { InboxItem, QuickReply } from "../../protocol";
import { useStore } from "../../store/store";
import { getClient } from "../../ws/client";
import { InboxItemCard } from "./InboxItemCard";
import styles from "./InboxView.module.css";

const ARCHIVED_LIMIT = 20;

export function InboxView() {
  const inbox = useStore((s) => s.inbox);
  const goToChat = useStore((s) => s.goToChat);
  const [archivedExpanded, setArchivedExpanded] = useState(false);

  const { open, archived } = useMemo(() => {
    const sorted = [...inbox].sort((a, b) => b.id - a.id); // newest first
    return {
      open: sorted.filter((i) => i.status === "open"),
      archived: sorted.filter((i) => i.status === "archived").slice(0, ARCHIVED_LIMIT),
    };
  }, [inbox]);

  function handleQuickReply(item: InboxItem, reply: QuickReply) {
    const localId = getClient()?.sendMessage(reply.value, item.anchor);
    goToChat({ scrollToMessageId: localId });
  }

  function handleReply(item: InboxItem) {
    goToChat({ composerDraft: { ref: item.anchor } });
  }

  function handleArchive(item: InboxItem) {
    getClient()?.archiveItem(item.id);
  }

  if (open.length === 0 && archived.length === 0) {
    return (
      <div className={styles.view}>
        <div className={styles.empty}>Nothing in the Inbox yet.</div>
      </div>
    );
  }

  return (
    <div className={styles.view}>
      <div className={styles.list}>
        {open.map((item) => (
          <InboxItemCard
            key={item.id}
            item={item}
            onQuickReply={handleQuickReply}
            onReply={handleReply}
            onArchive={handleArchive}
          />
        ))}
      </div>

      {archived.length > 0 && (
        <div className={styles.archivedSection}>
          <button
            type="button"
            className={styles.archivedToggle}
            onClick={() => setArchivedExpanded((v) => !v)}
          >
            {archivedExpanded ? "▾" : "▸"} Archived ({archived.length})
          </button>
          {archivedExpanded && (
            <div className={styles.list}>
              {archived.map((item) => (
                <InboxItemCard
                  key={item.id}
                  item={item}
                  onQuickReply={handleQuickReply}
                  onReply={handleReply}
                  onArchive={handleArchive}
                />
              ))}
            </div>
          )}
        </div>
      )}
    </div>
  );
}
