import { For, Show } from "solid-js";
import type { JSX } from "@solidjs/web";
import { SectionLabel } from "../components/ui/section-label";
import { ThreadLink } from "./ThreadRef";
import { threadState } from "./store";

export interface TaskConversationSlots {
  headline: () => JSX.Element;
  children: () => JSX.Element;
}

export function TaskShell(props: { taskId: number; conversation: (slots: TaskConversationSlots) => JSX.Element }) {
  const task = () => threadState.threads.find(thread => thread.id === props.taskId);
  const children = () => threadState.threads.filter(thread => thread.parent_thread_id === props.taskId && !thread.archived_at);
  return props.conversation({
    headline: () => <Show when={task()}>{current => <section data-slot="task-headline"><SectionLabel as="h2">Headline</SectionLabel><p class="text-base font-medium">{current().headline}</p><p class="text-xs text-muted-foreground">{current().status.kind.replaceAll("_", " ")} · {current().status.reason}</p></section>}</Show>,
    children: () => <Show when={children().length}><section data-slot="task-children"><SectionLabel as="h2">Children</SectionLabel><ul><For each={children()}>{child => <li class="min-h-11 py-2 text-sm"><ThreadLink id={child.id} /> <span class="text-muted-foreground">{child.headline}</span></li>}</For></ul></section></Show>,
  });
}
