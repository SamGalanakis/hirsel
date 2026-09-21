import { createEffect, createSignal, onCleanup, onSettled, Show } from "solid-js";
import type { JSX } from "@solidjs/web";
import { createMediaFlag } from "../lib/focus";
import { ThreadBoard } from "./ThreadBoard";
import { spaceBoardFitsBesideConversation } from "./space-board-layout";

export function SpaceChatShell(props: { spaceId: number; conversation: () => JSX.Element }) {
  const [tab, setTab] = createSignal<"chat" | "board">("chat");
  const desktop = createMediaFlag("(min-width: 900px)");
  const [beside, setBeside] = createSignal(false);
  let workspace: HTMLDivElement | undefined;
  let observer: ResizeObserver | undefined;
  const measure = (width = workspace?.clientWidth ?? 0) => {
    const rem = Number.parseFloat(getComputedStyle(document.documentElement).fontSize) || 16;
    setBeside(desktop() && spaceBoardFitsBesideConversation(width, rem));
  };
  onSettled(() => {
    measure();
    if (typeof ResizeObserver === "undefined") return;
    observer = new ResizeObserver(entries => measure(entries[0]?.contentRect.width));
    if (workspace) observer.observe(workspace);
  });
  createEffect(desktop, () => measure());
  onCleanup(() => observer?.disconnect());
  const chatVisible = () => beside() || tab() === "chat";
  const boardVisible = () => beside() || tab() === "board";
  const chatId = `space-chat-${props.spaceId}`;
  const boardId = `space-board-${props.spaceId}`;
  return <div ref={node => { workspace = node; }} data-slot="space-workspace" class={`flex min-h-0 min-w-0 flex-1 gap-2 ${beside() ? "flex-row" : "flex-col"}`}>
    <Show when={!beside()}><div data-slot="space-view-tabs" class="flex min-h-11 shrink-0 rounded-lg border border-border p-1" role="tablist" aria-label="Space view"><button class="min-h-11 flex-1 rounded-md text-sm aria-selected:bg-muted" role="tab" aria-controls={chatId} aria-selected={tab() === "chat" ? "true" : "false"} onClick={() => setTab("chat")}>Chat</button><button class="min-h-11 flex-1 rounded-md text-sm aria-selected:bg-muted" role="tab" aria-controls={boardId} aria-selected={tab() === "board" ? "true" : "false"} onClick={() => setTab("board")}>Board</button></div></Show>
    <Show when={chatVisible()}><div id={chatId} role={!beside() ? "tabpanel" : undefined} class="flex min-h-0 min-w-0 flex-1">{props.conversation()}</div></Show>
    <div id={boardId} role={!beside() ? "tabpanel" : undefined} data-slot="space-board-pane" class={`${boardVisible() ? "flex" : "hidden"} min-h-0 min-w-0 flex-1 ${beside() ? "min-w-80" : ""}`}><ThreadBoard spaceId={props.spaceId} visible={boardVisible} /></div>
  </div>;
}
