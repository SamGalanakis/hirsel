import type { InboxItem, QuickReply } from "../../protocol";
import { Markdown } from "../Markdown";
import { QuickReplyButtons } from "./QuickReplyButtons";
import styles from "./InboxItemCard.module.css";

interface Props {
  item: InboxItem;
  onQuickReply: (item: InboxItem, reply: QuickReply) => void;
  onReply: (item: InboxItem) => void;
  onArchive: (item: InboxItem) => void;
}

function formatTime(ts: string): string {
  try {
    return new Date(ts).toLocaleString([], {
      month: "short",
      day: "numeric",
      hour: "2-digit",
      minute: "2-digit",
    });
  } catch {
    return "";
  }
}

export function InboxItemCard({ item, onQuickReply, onReply, onArchive }: Props) {
  const isOpen = item.status === "open";

  return (
    <div
      className={`${styles.card} ${item.requires_response ? styles.requiresResponse : ""}`}
    >
      <div className={styles.header}>
        <span className={styles.timestamp}>{formatTime(item.ts)}</span>
        {!isOpen && <span className={styles.archivedTag}>Archived</span>}
      </div>
      <Markdown>{item.content}</Markdown>
      {isOpen && (
        <QuickReplyButtons
          quickReplies={item.quick_replies}
          onTap={(reply) => onQuickReply(item, reply)}
        />
      )}
      <div className={styles.actions}>
        <button type="button" className={styles.actionButton} onClick={() => onReply(item)}>
          Reply
        </button>
        {isOpen && (
          <button
            type="button"
            className={`${styles.actionButton} ${styles.archive}`}
            onClick={() => onArchive(item)}
          >
            Archive
          </button>
        )}
      </div>
    </div>
  );
}
