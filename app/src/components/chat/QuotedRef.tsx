import type { DisplayMessage } from "../../store/types";
import styles from "./QuotedRef.module.css";

interface Props {
  message: DisplayMessage | undefined;
  onTap: () => void;
}

function snippet(body: string): string {
  const oneLine = body.replace(/\s+/g, " ").trim();
  return oneLine.length > 80 ? `${oneLine.slice(0, 80)}…` : oneLine;
}

/** Compact quoted preview of a referenced ChatMessage, WhatsApp-quote style.
 * Tapping it scrolls to (and briefly highlights) the original. */
export function QuotedRef({ message, onTap }: Props) {
  if (!message) {
    return (
      <div className={styles.quote}>
        <span className={styles.missing}>original message unavailable</span>
      </div>
    );
  }

  return (
    <button type="button" className={styles.quote} onClick={onTap}>
      <div className={styles.author}>{message.author === "owner" ? "You" : "Agent"}</div>
      <div className={styles.snippet}>{snippet(message.body)}</div>
    </button>
  );
}
