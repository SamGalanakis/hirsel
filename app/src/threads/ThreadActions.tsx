import { For, Show } from "solid-js";
import { Dynamic } from "@solidjs/web";
import { Archive, Check, Clock, Copy, MoreHorizontal, RotateCcw, Square } from "../components/ui/icons";
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger } from "../components/ui/dropdown-menu";
import { threadActions, type ThreadActionIcon } from "./actions";
import type { Thread } from "./types";
const control = "inline-flex size-11 shrink-0 items-center justify-center rounded-lg text-muted-foreground hover:bg-muted hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring";
const icons = { settle: Check, reopen: RotateCcw, read: Check, snooze: Clock, archive: Archive, copy: Copy, stop: Square };
export function ThreadActionSymbol(props: { name: ThreadActionIcon }) { return <Dynamic component={icons[props.name]} class="size-4" />; }
export function ThreadActions(props: { thread: Thread; quick?: boolean; now?: number }) {
  const actions = () => threadActions(props.thread, props.now);
  return <div class="flex shrink-0 items-start">
    <Show when={props.quick}><button class={`${control} hidden min-[420px]:inline-flex`} aria-label={`${props.thread.settled_at ? "Reopen" : "Settle"} ${props.thread.title}`} title={props.thread.settled_at ? "Reopen thread" : "Settle thread"} onClick={() => actions().find(action => action.id === "settle")?.run()}><ThreadActionSymbol name={props.thread.settled_at ? "reopen" : "settle"} /></button></Show>
    <DropdownMenu>
      <DropdownMenuTrigger data-thread-actions={props.thread.id} class={control} aria-label={props.quick ? `Actions for ${props.thread.title}` : "Thread actions"} title="Thread actions"><MoreHorizontal class="size-4" /></DropdownMenuTrigger>
      <DropdownMenuContent><For each={actions()}>{action => <DropdownMenuItem class="min-h-11" onSelect={action.run}><ThreadActionSymbol name={action.icon} />{action.label}</DropdownMenuItem>}</For></DropdownMenuContent>
    </DropdownMenu>
  </div>;
}
