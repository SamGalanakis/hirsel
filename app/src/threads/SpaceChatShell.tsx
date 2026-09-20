import { createSignal, Show } from "solid-js";
import type { JSX } from "@solidjs/web";
import { createMediaFlag } from "../lib/focus";
import { ThreadBoard } from "./ThreadBoard";

export function SpaceChatShell(props: { spaceId: number; conversation: () => JSX.Element }) {
  const [tab, setTab] = createSignal<"chat" | "board">("chat");
  const desktop = createMediaFlag("(min-width: 900px)");
  return <div class="flex min-h-0 min-w-0 flex-1 flex-col gap-2 split:flex-row">
    <div class="flex min-h-11 shrink-0 rounded-lg border border-border p-1 split:hidden" role="tablist" aria-label="Space view"><button class="min-h-11 flex-1 rounded-md text-sm aria-selected:bg-muted" role="tab" aria-selected={tab() === "chat" ? "true" : "false"} onClick={() => setTab("chat")}>Chat</button><button class="min-h-11 flex-1 rounded-md text-sm aria-selected:bg-muted" role="tab" aria-selected={tab() === "board" ? "true" : "false"} onClick={() => setTab("board")}>Board</button></div>
    <Show when={tab() === "chat"}><div class="flex min-h-0 min-w-0 flex-1">{props.conversation()}</div></Show>
    <div class={tab() === "board" ? "flex min-h-0 min-w-0 flex-1" : "hidden min-h-0 min-w-0 flex-1 split:flex"}><ThreadBoard spaceId={props.spaceId} visible={() => desktop() || tab() === "board"} /></div>
  </div>;
}
