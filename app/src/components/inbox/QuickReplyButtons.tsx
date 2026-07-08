import type { QuickReply } from "../../protocol";
import styles from "./QuickReplyButtons.module.css";

interface Props {
  quickReplies: QuickReply[];
  onTap: (reply: QuickReply) => void;
}

export function QuickReplyButtons({ quickReplies, onTap }: Props) {
  if (quickReplies.length === 0) return null;

  return (
    <div className={styles.row}>
      {quickReplies.map((qr) => (
        <button
          key={qr.value}
          type="button"
          className={styles.button}
          onClick={() => onTap(qr)}
        >
          {qr.label}
        </button>
      ))}
    </div>
  );
}
