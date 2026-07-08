import type { DisplayMessage } from "../../store/types";
import { Markdown } from "../Markdown";
import { QuotedRef } from "./QuotedRef";
import styles from "./MessageBubble.module.css";

interface Props {
  message: DisplayMessage;
  refTarget: DisplayMessage | undefined;
  highlighted: boolean;
  onTapQuote: (id: number) => void;
}

function formatTime(ts: string): string {
  try {
    return new Date(ts).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
  } catch {
    return "";
  }
}

export function MessageBubble({ message, refTarget, highlighted, onTapQuote }: Props) {
  return (
    <div
      id={`msg-${message.id}`}
      className={`${styles.row} ${styles[message.author]} ${highlighted ? styles.highlight : ""}`}
    >
      <div className={styles.bubble}>
        {message.ref !== null && (
          <QuotedRef message={refTarget} onTap={() => onTapQuote(message.ref!)} />
        )}
        <Markdown>{message.body}</Markdown>
        <div className={styles.meta}>
          {message.pending && <span className={styles.pending}>sending…</span>}
          <span>{formatTime(message.ts)}</span>
        </div>
      </div>
    </div>
  );
}
