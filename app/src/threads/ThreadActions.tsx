import { createSignal, For, Show } from "solid-js";
import { Dynamic } from "@solidjs/web";
import { ArrowLeft, Pin, GitBranch, Archive, Check, ChevronRight, Clock, Copy, Layers, MoreHorizontal, RotateCcw, Square, SquarePen } from "../components/ui/icons";
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuSeparator, DropdownMenuTrigger } from "../components/ui/dropdown-menu";
import { threadActions, type ThreadActionIcon, type ThreadActionItem } from "./actions";
import type { Thread } from "./types";
const control = "shrink-0 items-center justify-center rounded-lg text-muted-foreground hover:bg-muted hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring";
/** A dense row keeps its 28px look on a fine pointer; touch gets the 44px the
 * thumb needs as an invisible pseudo-element around the same glyph, so the
 * inventory never grows a phone-sized row on the desktop. */
const touchTarget = "relative pointer-coarse:before:absolute pointer-coarse:before:left-1/2 pointer-coarse:before:top-1/2 pointer-coarse:before:size-11 pointer-coarse:before:-translate-x-1/2 pointer-coarse:before:-translate-y-1/2 pointer-coarse:before:content-['']";
const icons = { icon: SquarePen, pin: Pin, child: GitBranch, settle: Check, reopen: RotateCcw, read: Check, snooze: Clock, archive: Archive, copy: Copy, stop: Square, kind: Layers };
export function ThreadActionSymbol(props: { name: ThreadActionIcon }) { return <Dynamic component={icons[props.name]} class="size-4" />; }
export function ThreadActions(props: { thread: Thread; quick?: boolean; dense?: boolean; now?: number }) {
  const actions = () => threadActions(props.thread, props.now);
  /** The one action whose choices are open in the panel, or null for the root
   * menu. A drill-in keeps the whole contract on the existing menu primitive:
   * one panel, one roving focus, Escape still closes. */
  const [submenu, setSubmenu] = createSignal<ThreadActionItem | null>(null);
  const size = () => props.dense ? `size-7 ${touchTarget}` : "size-11";
  return <div class="flex shrink-0 items-start">
    <Show when={props.quick && props.thread.kind === "task"}><button class={`${control} ${size()} hidden min-[420px]:inline-flex`} aria-label={`${props.thread.settled_at ? "Reopen" : "Mark done"} ${props.thread.title}`} title={props.thread.settled_at ? "Reopen task" : "Mark task done"} onClick={() => actions().find(action => action.id === "settle")?.run()}><Dynamic component={props.thread.settled_at ? icons.reopen : icons.settle} class={props.dense ? "size-3.5" : "size-4"} /></button></Show>
    <DropdownMenu onOpenChange={open => { if (!open) setSubmenu(null); }}>
      <DropdownMenuTrigger data-thread-actions={props.thread.id} class={`${control} ${size()} inline-flex`} aria-label={props.quick ? `Actions for ${props.thread.title}` : "Thread actions"} title="Thread actions"><MoreHorizontal class={props.dense ? "size-3.5" : "size-4"} /></DropdownMenuTrigger>
      <DropdownMenuContent>
        <Show when={submenu()} fallback={<For each={actions()}>{(action, index) => <>
          <Show when={index() > 0 && actions()[index() - 1].group !== action.group}><DropdownMenuSeparator /></Show>
          <Show when={action.options} fallback={
            <DropdownMenuItem class="min-h-11" variant={action.destructive ? "destructive" : "default"} onSelect={action.run}><ThreadActionSymbol name={action.icon} />{action.label}</DropdownMenuItem>
          }>{options => <button type="button" role="menuitem" tabindex={-1} data-slot="dropdown-menu-item" aria-haspopup="menu" aria-expanded="false"
            class="relative flex min-h-11 w-full cursor-default select-none items-center gap-2 rounded-sm px-2 py-1.5 text-left text-sm outline-none transition-colors hover:bg-accent hover:text-accent-foreground focus:bg-accent focus:text-accent-foreground [&_svg]:size-4 [&_svg]:shrink-0"
            onClick={() => setSubmenu(action)}><ThreadActionSymbol name={action.icon} /><span class="flex-1">{action.label}</span><ChevronRight class="text-muted-foreground" /><span class="sr-only">{options().length} options</span></button>}</Show>
        </>}</For>}>{open => <>
          <button type="button" role="menuitem" tabindex={-1} data-slot="dropdown-menu-item" class="relative flex min-h-11 w-full cursor-default select-none items-center gap-2 rounded-sm px-2 py-1.5 text-left text-sm text-muted-foreground outline-none transition-colors hover:bg-accent focus:bg-accent [&_svg]:size-4 [&_svg]:shrink-0" onClick={() => setSubmenu(null)}><ArrowLeft />{open().label}</button>
          <DropdownMenuSeparator />
          <For each={open().options}>{option => <DropdownMenuItem class="min-h-11 pl-8" onSelect={option.run}>{option.label}</DropdownMenuItem>}</For>
        </>}</Show>
      </DropdownMenuContent>
    </DropdownMenu>
  </div>;
}
