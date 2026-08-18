import { Check, ChevronRight, Settings2, X } from "lucide-solid";
import { createSignal, For, Show } from "solid-js";
import type { ToolCall } from "../../protocol";

/** Committed tool-call chip (v1.4) in an agent bubble's footer: a collapsed
 * "⚙ N tools" pill that expands inline to the per-tool name + ok (check/cross)
 * list. Default collapsed; renders nothing when the list is empty. */
export function CommittedToolCalls(props: { toolCalls: ToolCall[] }) {
  const [expanded, setExpanded] = createSignal(false);
  const count = () => props.toolCalls.length;

  return (
    <Show when={count() > 0}>
      <div class="flex flex-col gap-1" data-slot="committed-tool-calls">
        <button
          type="button"
          class="-ml-1 inline-flex w-fit items-center gap-1 rounded px-1 py-px text-xs text-muted-foreground transition-colors hover:bg-muted/40 hover:text-foreground"
          aria-expanded={expanded()}
          aria-label={`${count()} tool call${count() === 1 ? "" : "s"}`}
          onClick={() => setExpanded((v) => !v)}
        >
          <ChevronRight
            class="size-3 shrink-0 transition-transform"
            classList={{ "rotate-90": expanded() }}
            aria-hidden="true"
          />
          <Settings2 class="size-3 shrink-0" aria-hidden="true" />
          {count()} {count() === 1 ? "tool" : "tools"}
        </button>
        <Show when={expanded()}>
          <ul class="ml-1 flex flex-col gap-0.5 border-l border-border/60 pl-2.5">
            <For each={props.toolCalls}>
              {(tc) => (
                <li class="flex items-center gap-1.5 text-[0.72rem]">
                  <Show
                    when={tc.ok}
                    fallback={<X class="size-3 shrink-0 text-destructive" aria-label="failed" />}
                  >
                    <Check class="size-3 shrink-0 text-status-success" aria-label="ok" />
                  </Show>
                  <span class="font-mono text-foreground/90">{tc.name}</span>
                </li>
              )}
            </For>
          </ul>
        </Show>
      </div>
    </Show>
  );
}
