import { Check, ChevronRight, Settings2, Wrench, X } from "lucide-solid";
import { createSignal, For, Show } from "solid-js";
import type { ToolCall } from "../../protocol";
import type { LiveToolCall } from "../../store/types";

/** Live tool-call rows (v1.4) rendered under the "Thinking…" marker while a turn
 * runs. Ephemeral — the store clears them on turn commit. Compact rows of
 * icon + name + summary; the newest (last, highest seq) row shimmers to signal
 * it is the one in flight. Mirrors the ported shadcn "tool activity" pattern. */
export function LiveToolCalls(props: { calls: LiveToolCall[] }) {
  return (
    <ul class="ml-1 flex flex-col gap-1 border-l border-border/60 pl-3" data-slot="live-tool-calls">
      <For each={props.calls}>
        {(call, i) => {
          const newest = () => i() === props.calls.length - 1;
          return (
            <li class="flex min-w-0 items-center gap-1.5 text-xs text-muted-foreground">
              <Wrench class="size-3 shrink-0 text-status-active" aria-hidden="true" />
              <span
                class="shrink-0 font-mono text-[0.72rem] text-foreground/90"
                classList={{ shimmer: newest() }}
              >
                {call.name}
              </span>
              <Show when={call.summary}>
                <span class="min-w-0 truncate" classList={{ shimmer: newest() }}>
                  {call.summary}
                </span>
              </Show>
            </li>
          );
        }}
      </For>
    </ul>
  );
}

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
          class="inline-flex w-fit items-center gap-1 rounded-full bg-muted px-1.5 py-px text-[0.68rem] text-muted-foreground transition-colors hover:text-foreground"
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
