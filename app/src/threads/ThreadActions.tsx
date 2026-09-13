import { For, Show } from "solid-js";
import { Dynamic } from "@solidjs/web";
import { Pin, GitBranch, Archive, Check, Clock, Copy, Layers, MoreHorizontal, RotateCcw, Square, SquarePen } from "../components/ui/icons";
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger } from "../components/ui/dropdown-menu";
import { threadActions, type ThreadActionIcon } from "./actions";
import type { Thread } from "./types";
const control = "shrink-0 items-center justify-center rounded-lg text-muted-foreground hover:bg-muted hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring";
const icons = { icon: SquarePen, pin: Pin, child: GitBranch, settle: Check, reopen: RotateCcw, read: Check, snooze: Clock, archive: Archive, copy: Copy, stop: Square, kind: Layers };
export function ThreadActionSymbol(props: { name: ThreadActionIcon }) { return <Dynamic component={icons[props.name]} class="size-4" />; }
export function ThreadActions(props: { thread: Thread; quick?: boolean; dense?: boolean; now?: number }) {
  const actions = () => threadActions(props.thread, props.now);
  /** Dense inventory rows carry the same actions at inventory scale; a coarse
   * pointer keeps the 44px target everywhere. */
  const size = () => props.dense ? "size-7 pointer-coarse:size-11" : "size-11";
  return <div class="flex shrink-0 items-start">
    <Show when={props.quick && props.thread.kind === "task"}><button class={`${control} ${size()} hidden min-[420px]:inline-flex`} aria-label={`${props.thread.settled_at ? "Reopen" : "Mark done"} ${props.thread.title}`} title={props.thread.settled_at ? "Reopen task" : "Mark task done"} onClick={() => actions().find(action => action.id === "settle")?.run()}><Dynamic component={props.thread.settled_at ? icons.reopen : icons.settle} class={props.dense ? "size-3.5" : "size-4"} /></button></Show>
    <DropdownMenu>
      <DropdownMenuTrigger data-thread-actions={props.thread.id} class={`${control} ${size()} inline-flex`} aria-label={props.quick ? `Actions for ${props.thread.title}` : "Thread actions"} title="Thread actions"><MoreHorizontal class={props.dense ? "size-3.5" : "size-4"} /></DropdownMenuTrigger>
      <DropdownMenuContent><For each={actions()}>{action => <DropdownMenuItem class="min-h-11" onSelect={action.run}><ThreadActionSymbol name={action.icon} />{action.label}</DropdownMenuItem>}</For></DropdownMenuContent>
    </DropdownMenu>
  </div>;
}
