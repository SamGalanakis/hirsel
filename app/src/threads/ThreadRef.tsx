import { createMemo, For, Show } from "solid-js";

import { splitTaskRefs } from "../lib/task-ref";
import { focusThread, threadState } from "./store";
export function ThreadRefText(props: { value: string }) {
  const spans = createMemo(() => splitTaskRefs(props.value, id => threadState.threads.some(t => t.id === id)));
  return <For each={spans()}>{span => <Show when={span.taskId !== null} fallback={span.text}><button type="button" class="underline decoration-current/40 underline-offset-2" title={threadState.threads.find(t => t.id === span.taskId)?.title} onClick={() => focusThread(span.taskId!)}>{span.text}</button></Show>}</For>;
}
