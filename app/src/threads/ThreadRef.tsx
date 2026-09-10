import { historyId } from "../lib/history";
import { threadUrl } from "../lib/thread-url";
import { RichLink } from "../related/RichLink";
import { useRelatedOrigin } from "../related/context";
import { createMemo, For, Show } from "solid-js";

import { splitThreadRefs } from "../lib/thread-ref";
import { threadState } from "./store";
export function ThreadRefText(props: { value: string }) {
  const origin = useRelatedOrigin();
  const messageHistory = origin?.historyId ?? historyId();
  const spans = createMemo(() => splitThreadRefs(props.value, id => threadState.ready && messageHistory === historyId() && threadState.threads.some(t => t.id === id)));
  return <For each={spans()}>{span => <Show when={span.threadId !== null && messageHistory} fallback={span.text}><RichLink href={threadUrl({kind:"thread",history_id:messageHistory!,thread_id:span.threadId!})} label={span.text} target={{kind:"thread",history_id:messageHistory!,thread_id:span.threadId!}}>{span.text}</RichLink></Show>}</For>;
}

/** Exact human destination; missing IDs remain visible, never retargeted. */
export function ThreadLink(props: { id: number }) {
  const thread = () => threadState.ready && referenceHistory === historyId() ? threadState.threads.find(thread => thread.id === props.id) : undefined;
  const origin = useRelatedOrigin();
  const referenceHistory = origin?.historyId ?? historyId();
  const target = () => ({kind:"thread" as const,history_id:referenceHistory!,thread_id:props.id});
  const label = () => `#${props.id} ${thread()?.title ?? "Unavailable thread"}`;
  return <Show when={referenceHistory} fallback={label()}><RichLink href={threadUrl(target())} label={label()} target={target()}>{label()}</RichLink></Show>;
}
