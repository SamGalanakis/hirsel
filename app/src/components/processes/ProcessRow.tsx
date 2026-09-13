import { Activity, ChevronDown } from "@/components/ui/icons";
import { createSignal, Show } from "solid-js";

import type { ProcessInfo, ProcessState } from "../../protocol";
import { formatRelativeTime } from "../../lib/format";
import { ThreadLink } from "../../threads/ThreadRef";
import { Card } from "../ui/card";

interface Props {
  process: ProcessInfo;
  onCancel?: (process: ProcessInfo) => void;
  onDisableTrigger?: (process: ProcessInfo) => void;
}

const STATE_LABEL: Record<ProcessState, string> = {
  running: "running",
  waiting: "waiting",
  done: "done",
  failed: "failed",
  cancelled: "cancelled",
  abandoned: "abandoned",
  caller_departed: "caller left",
};

/** State chip. `running` pulses subtly; `failed`/`abandoned` carry a warning
 * tint; `done`/`cancelled` are quiet. */
function StateChip(props: { state: ProcessState }) {
  const warn = () => props.state === "failed" || props.state === "abandoned";
  const running = () => props.state === "running" || props.state === "waiting";
  return (
    <span
      class={["inline-flex shrink-0 items-center gap-1 rounded-full px-1.5 py-px text-xs font-medium", {
        "bg-status-active/15 text-status-active": running(),
        "bg-status-danger/15 text-status-danger": props.state === "failed",
        "bg-status-attention/15 text-status-attention": props.state === "abandoned",
        "bg-muted text-muted-foreground": props.state === "done" || props.state === "cancelled" || props.state === "caller_departed",
      }]}

      data-state={props.state}
    >
      <Show when={running()}>
        <span class="size-1.5 animate-pulse rounded-full bg-status-active" aria-hidden="true" />
      </Show>
      {STATE_LABEL[props.state]}
      <Show when={warn()}>
        <span class="sr-only"> (needs attention)</span>
      </Show>
    </span>
  );
}

/** Row-mode state: the literal word plus one semantic dot — status as text +
 * a single mark, never a repeated pill (the promoted card keeps the fuller
 * StateChip). */
function StateMark(props: { state: ProcessState }) {
  return (
    <span
      class="inline-flex shrink-0 items-center gap-1.5 text-xs text-muted-foreground"
      data-state={props.state}
    >
      <span
        class={["size-1.5 rounded-full", {
          "bg-status-active": props.state === "running" || props.state === "waiting",
          "bg-status-danger": props.state === "failed",
          "bg-status-attention": props.state === "abandoned",
          "bg-muted-foreground": props.state === "done" || props.state === "cancelled" || props.state === "caller_departed",
        }]}

        aria-hidden="true"
      />
      {STATE_LABEL[props.state]}
    </span>
  );
}

/** The one disclosure mark in this list: a chevron that ROTATES between closed
 * and open. A `›` pointing off the right edge promises navigation to somewhere
 * else; these rows unfold in place, so it lied. One mark, both presentations. */
function DisclosureChevron(props: { expanded: boolean; class?: string }) {
  return (
    <ChevronDown
      aria-hidden="true"
      class={[`size-4 shrink-0 text-muted-foreground transition-transform duration-200 ease-out ${props.class ?? ""}`, { "-rotate-90": !props.expanded }]}

    />
  );
}

export function ProcessRow(props: Props) {
  const [expanded, setExpanded] = createSignal(false);
  const p = () => props.process;
  const running = () => p().state === "running" || p().state === "waiting";
  // Resting (finished) processes render as dense hairline rows; the active
  // (running) process — or one the Owner taps open — is promoted to a card.
  const asCard = () => running() || expanded();

  const Row = () => (
    <div
      class="flex min-h-11 items-center gap-2 border-b border-border/60 px-3"
      data-slot="process-row"
      data-state={p().state}
    >
      <button
        type="button"
        class="flex min-w-0 flex-1 items-center gap-2 py-2.5 text-left [@media(pointer:coarse)]:min-h-11"
        aria-expanded={(expanded()) ? "true" : "false"}
        aria-label={`Show details for ${p().name}`}
        onClick={() => setExpanded((v) => !v)}
      >
        <span class="shrink-0 text-muted-foreground">
          <Activity class="size-4" aria-label="Process" />
        </span>
        <code class="min-w-0 flex-1 truncate rounded bg-muted px-1 py-0.5 font-mono text-meta text-muted-foreground">
          {p().name}
        </code>
        <span class="shrink-0 text-meta tabular-nums text-muted-foreground">
          {formatRelativeTime(p().started_ts)}
        </span>
      </button>
      <StateMark state={p().state} />
      <DisclosureChevron expanded={expanded()} />
    </div>
  );

  return (
    <Show when={asCard()} fallback={<Row />}>
    <Card
      size="sm"
      class="mx-3 gap-2 px-3 py-3"
      data-slot="process-row"
      data-state={p().state}
    >
      <button
        type="button"
        class="flex min-h-11 w-full items-start gap-2 text-left"
        aria-expanded={(expanded()) ? "true" : "false"}
        aria-label={`Show details for ${p().name}`}
        onClick={() => setExpanded((v) => !v)}
      >
        <span class="mt-0.5 shrink-0 text-muted-foreground">
          <Activity class="size-4" aria-label="Process" />
        </span>

        <span class="flex min-w-0 flex-1 flex-col gap-1">
          <span class="flex items-start justify-between gap-2">
            <code class="min-w-0 flex-1 truncate rounded bg-muted px-1 py-0.5 font-mono text-meta text-foreground/90">
              {p().name}
            </code>
            <StateChip state={p().state} />
          </span>

          <span class="flex flex-wrap items-center gap-x-2 text-xs text-muted-foreground">
            <span>started {formatRelativeTime(p().started_ts)}</span>
            <Show when={p().last_fired_ts}>
              <span aria-hidden="true">·</span>
              <span>last fired {formatRelativeTime(p().last_fired_ts!)}</span>
            </Show>
          </span>

          <Show when={p().last_outcome}>
            <span class="min-w-0 truncate text-meta text-muted-foreground">{p().last_outcome}</span>
          </Show>
        </span>

        <DisclosureChevron expanded={expanded()} class="mt-0.5" />
      </button>

      <div class="text-xs text-muted-foreground"><ThreadLink id={p().thread_id} /></div>
      {/* Expanded detail. */}
      <Show when={expanded()}>
        <div class="ml-6 flex flex-col gap-2 border-l border-border/60 pl-3 pt-1">
          <div class="flex flex-col gap-0.5">
            <span class="text-xs font-medium text-muted-foreground">Trigger</span>
            <code class="rounded bg-muted px-1.5 py-1 font-mono text-meta text-foreground/90 wrap-break-word">
              {p().trigger ?? "direct start"}
            </code>
          </div>

          <Show when={p().last_outcome}>
            <div class="flex flex-col gap-0.5">
              <span class="text-xs font-medium text-muted-foreground">
                Latest
              </span>
              <span class="text-[0.78rem] text-foreground/90 wrap-break-word">{p().last_outcome}</span>
            </div>
          </Show>

          <div class="flex flex-wrap gap-x-4 gap-y-0.5 text-xs text-muted-foreground">
            <span>Started {formatRelativeTime(p().started_ts)}</span>
            <span>Updated {formatRelativeTime(p().last_event_ts)}</span>
          </div>
          <div class="flex flex-wrap gap-2">
            <Show when={p().state === "running" && p().active_process_id && p().cancellable && props.onCancel}>
              <button type="button" class="rounded border border-border px-2 py-1 text-xs" onClick={() => props.onCancel?.(p())}>
                Cancel process
              </button>
            </Show>
            <Show when={p().trigger_recurring && p().trigger_enabled && p().trigger_subscription_key && p().trigger_revision !== null && props.onDisableTrigger}>
              <button type="button" class="rounded border border-border px-2 py-1 text-xs" onClick={() => props.onDisableTrigger?.(p())}>
                Disable trigger
              </button>
            </Show>
          </div>
        </div>
      </Show>
    </Card>
    </Show>
  );
}
