import { useEffect, useMemo, useRef, useState } from "react";
import { useStore } from "../../store/store";
import { getClient } from "../../ws/client";
import { MessageBubble } from "./MessageBubble";
import { AgentActivityIndicator } from "./AgentActivityIndicator";
import { Composer } from "./Composer";
import styles from "./ChatView.module.css";

const HIGHLIGHT_MS = 1600;

export function ChatView() {
  const messages = useStore((s) => s.messages);
  const scrollToMessageId = useStore((s) => s.scrollToMessageId);
  const clearScrollTarget = useStore((s) => s.clearScrollTarget);
  const composerDraft = useStore((s) => s.composerDraft);
  const clearComposerDraft = useStore((s) => s.clearComposerDraft);

  const scrollRef = useRef<HTMLDivElement>(null);
  const [highlightedId, setHighlightedId] = useState<number | null>(null);
  const prevLength = useRef(0);

  const messagesById = useMemo(() => {
    const map = new Map<number, (typeof messages)[number]>();
    for (const m of messages) map.set(m.id, m);
    return map;
  }, [messages]);

  function scrollToId(id: number) {
    document.getElementById(`msg-${id}`)?.scrollIntoView({ behavior: "smooth", block: "center" });
    setHighlightedId(id);
    setTimeout(() => setHighlightedId((cur) => (cur === id ? null : cur)), HIGHLIGHT_MS);
  }

  // Auto-scroll to the newest message as the thread grows, unless a specific
  // scroll target was requested (handled by the effect below).
  useEffect(() => {
    if (messages.length > prevLength.current && scrollToMessageId === null) {
      scrollRef.current?.scrollTo({ top: scrollRef.current.scrollHeight, behavior: "smooth" });
    }
    prevLength.current = messages.length;
  }, [messages, scrollToMessageId]);

  useEffect(() => {
    if (scrollToMessageId === null) return;
    scrollToId(scrollToMessageId);
    clearScrollTarget();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [scrollToMessageId, clearScrollTarget]);

  const replyingTo = composerDraft ? messagesById.get(composerDraft.ref) : null;

  function handleSend(body: string, ref: number | null) {
    getClient()?.sendMessage(body, ref);
  }

  return (
    <div className={styles.view}>
      <div className={styles.scroll} ref={scrollRef}>
        {messages.length === 0 && (
          <div className={styles.empty}>No messages yet. Say hello to the Agent.</div>
        )}
        {messages.map((m) => (
          <MessageBubble
            key={m.clientId ?? m.id}
            message={m}
            refTarget={m.ref !== null ? messagesById.get(m.ref) : undefined}
            highlighted={highlightedId === m.id}
            onTapQuote={scrollToId}
          />
        ))}
      </div>
      <AgentActivityIndicator />
      <Composer replyingTo={replyingTo} onCancelReply={clearComposerDraft} onSend={handleSend} />
    </div>
  );
}
