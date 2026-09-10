import { createSignal, Show } from "solid-js";
import { openThread, setThreadState, threadState } from "./store";
export function ThreadError(props: { threadId?: number; navigation?: boolean }) {
  const [retrying, setRetrying] = createSignal(false);
  const error = () => {
    const failure = threadState.error;
    if (!failure) return null;
    if (props.navigation) return failure.operation === "request" ? failure : null;
    return failure.threadId === undefined || failure.threadId === props.threadId ? failure : null;
  };
  const retry = async () => {
    const failure = error(); if (failure?.operation !== "load" || failure.threadId === undefined) return;
    setRetrying(true); try { await openThread(failure.threadId, failure.beforeId); } catch { /* Keep contextual failure visible. */ } finally { setRetrying(false); }
  };
  return <Show when={error()}>{failure => <div role="alert" class="mx-auto w-full max-w-measure space-y-2 px-3 py-3 text-sm">
    <p>{failure().operation === "load" ? "Couldn’t load this conversation. Your draft is kept." : failure().operation === "send" ? "Your message wasn’t confirmed. Use Retry beside the message to send it safely." : "Hirsel couldn’t complete that request. Your conversation is kept."}</p>
    <div class="flex gap-2"><Show when={failure().operation === "load"}><button class="min-h-11 rounded-lg px-3 hover:bg-muted focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring" disabled={retrying()} onClick={() => void retry()}>{retrying() ? "Loading…" : "Retry loading conversation"}</button></Show><button class="min-h-11 rounded-lg px-3 text-muted-foreground hover:bg-muted focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring" onClick={() => setThreadState(draft => { draft.error = null; })}>Dismiss</button></div>
    <details><summary class="cursor-pointer py-2 text-muted-foreground">Technical details</summary><p class="break-words">{failure().detail}</p></details>
  </div>}</Show>;
}
