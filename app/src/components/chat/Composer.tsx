import { useEffect, useRef, useState } from "react";
import type { KeyboardEvent } from "react";
import type { DisplayMessage } from "../../store/types";
import styles from "./Composer.module.css";

const MAX_HEIGHT_PX = 112;

interface Props {
  replyingTo: DisplayMessage | undefined | null;
  onCancelReply: () => void;
  onSend: (body: string, ref: number | null) => void;
}

function snippet(body: string): string {
  const oneLine = body.replace(/\s+/g, " ").trim();
  return oneLine.length > 60 ? `${oneLine.slice(0, 60)}…` : oneLine;
}

/** Composer pinned above the tab bar. Enter inserts a newline (mobile
 * default); Ctrl/Cmd+Enter sends (desktop convenience). */
export function Composer({ replyingTo, onCancelReply, onSend }: Props) {
  const [value, setValue] = useState("");
  const textareaRef = useRef<HTMLTextAreaElement>(null);

  useEffect(() => {
    const el = textareaRef.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = `${Math.min(el.scrollHeight, MAX_HEIGHT_PX)}px`;
  }, [value]);

  useEffect(() => {
    if (replyingTo) textareaRef.current?.focus();
  }, [replyingTo]);

  function send() {
    const body = value.trim();
    if (body.length === 0) return;
    onSend(body, replyingTo?.id ?? null);
    setValue("");
    if (replyingTo) onCancelReply();
  }

  function handleKeyDown(e: KeyboardEvent<HTMLTextAreaElement>) {
    if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) {
      e.preventDefault();
      send();
    }
  }

  return (
    <div className={styles.wrap}>
      {replyingTo && (
        <div className={styles.replyChip}>
          <div className={styles.replyChipBody}>
            <div className={styles.replyChipLabel}>
              Replying to {replyingTo.author === "owner" ? "you" : "Agent"}
            </div>
            <div className={styles.replyChipSnippet}>{snippet(replyingTo.body)}</div>
          </div>
          <button
            type="button"
            className={styles.replyChipCancel}
            onClick={onCancelReply}
            aria-label="Cancel reply"
          >
            ×
          </button>
        </div>
      )}
      <div className={styles.row}>
        <textarea
          ref={textareaRef}
          className={styles.textarea}
          rows={1}
          placeholder="Message the Agent…"
          value={value}
          onChange={(e) => setValue(e.target.value)}
          onKeyDown={handleKeyDown}
        />
        <button
          type="button"
          className={styles.send}
          onClick={send}
          disabled={value.trim().length === 0}
          aria-label="Send"
        >
          ↑
        </button>
      </div>
      <div className={styles.hint}>Enter for newline · ⌘/Ctrl+Enter to send</div>
    </div>
  );
}
