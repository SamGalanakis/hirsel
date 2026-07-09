import { Bot, ChevronDown, ChevronRight, OctagonX, Radar } from "lucide-solid";
import { createSignal, Show } from "solid-js";
import type { ProcessInfo, ProcessState } from "../../protocol";
import { formatRelativeTime } from "../../lib/format";
import { Card } from "../ui/card";

interface Props {
  process: ProcessInfo;
  /** "Ask to stop": switch to Chat with the composer pre-filled. Subagent only,
   * while running — interrupts route through the Agent, never a direct kill. */
  onAskToStop: (process: ProcessInfo) => void;
}

const STATE_LABEL: Record<ProcessState, string> = {
  running: "running",
  done: "done",
  failed: "failed",
  cancelled: "cancelled",
  abandoned: "abandoned",
};

/** State chip. `running` pulses subtly; `failed`/`abandoned` carry a warning
 * tint; `done`/`cancelled` are quiet. */
function StateChip(props: { state: ProcessState }) {
  const warn = () => props.state === "failed" || props.state === "abandoned";
  const running = () => props.state === "running";
  return (
    <span
      class="inline-flex shrink-0 items-center gap-1 rounded-full px-1.5 py-px text-[0.62rem] font-medium tracking-[0.02em]"
      classList={{
        "bg-status-active/15 text-status-active": running(),
        "bg-status-danger/15 text-status-danger": props.state === "failed",
        "bg-status-attention/15 text-status-attention": props.state === "abandoned",
        "bg-muted text-muted-foreground": props.state === "done" || props.state === "cancelled",
      }}
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

export function ProcessRow(props: Props) {
  const [expanded, setExpanded] = createSignal(false);
  const p = () => props.process;
  const isSubagent = () => p().kind === "subagent";
  const isMonitor = () => p().kind === "monitor";
  const running = () => p().state === "running";
  // A monitor has "fired" once its last event is distinct from its start.
  const hasFired = () => p().last_event_ts !== p().started_ts;

  return (
    <Card
      size="sm"
      class="mx-3 gap-2 px-3 py-3"
      data-slot="process-row"
      data-kind={p().kind}
      data-state={p().state}
    >
      <button
        type="button"
        class="flex w-full items-start gap-2 text-left"
        aria-expanded={expanded()}
        onClick={() => setExpanded((v) => !v)}
      >
        <span class="mt-0.5 shrink-0 text-muted-foreground">
          <Show when={isSubagent()} fallback={<Radar class="size-4" aria-label="Monitor" />}>
            <Bot class="size-4" aria-label="Sub-agent" />
          </Show>
        </span>

        <span class="flex min-w-0 flex-1 flex-col gap-1">
          {/* Title line: label (prompt snippet for subagents; cmd, code-style,
              for monitors) + state chip. */}
          <span class="flex items-start justify-between gap-2">
            <Show
              when={isMonitor()}
              fallback={
                <span class="min-w-0 flex-1 truncate text-sm font-medium text-foreground">
                  {p().label}
                </span>
              }
            >
              <code class="min-w-0 flex-1 truncate rounded bg-muted px-1 py-0.5 font-mono text-[0.72rem] text-foreground/90">
                {p().label}
              </code>
            </Show>
            <StateChip state={p().state} />
          </span>

          {/* Sub-agent chips: agent + model. */}
          <Show when={isSubagent() && (p().agent || p().model)}>
            <span class="flex flex-wrap items-center gap-1">
              <Show when={p().agent}>
                <span class="rounded bg-muted px-1.5 py-px text-[0.62rem] font-medium text-muted-foreground">
                  {p().agent}
                </span>
              </Show>
              <Show when={p().model}>
                <span class="rounded bg-muted px-1.5 py-px text-[0.62rem] text-muted-foreground">
                  {p().model}
                </span>
              </Show>
            </span>
          </Show>

          {/* Meta line: relative start (+ last-fired for a monitor that fired). */}
          <span class="flex flex-wrap items-center gap-x-2 text-[0.68rem] text-muted-foreground">
            <span>started {formatRelativeTime(p().started_ts)}</span>
            <Show when={isMonitor() && hasFired()}>
              <span aria-hidden="true">·</span>
              <span>last fired {formatRelativeTime(p().last_event_ts)}</span>
            </Show>
          </span>

          {/* Latest summary line (single line, truncated). */}
          <Show when={p().summary}>
            <span class="min-w-0 truncate text-[0.72rem] text-muted-foreground">{p().summary}</span>
          </Show>
        </span>

        <span class="mt-0.5 shrink-0 text-muted-foreground">
          <Show when={expanded()} fallback={<ChevronRight class="size-4" aria-hidden="true" />}>
            <ChevronDown class="size-4" aria-hidden="true" />
          </Show>
        </span>
      </button>

      {/* Expanded detail. */}
      <Show when={expanded()}>
        <div class="ml-6 flex flex-col gap-2 border-l border-border/60 pl-3 pt-1">
          <div class="flex flex-col gap-0.5">
            <span class="text-[0.62rem] uppercase tracking-[0.04em] text-muted-foreground">
              {isMonitor() ? "Probe" : "Prompt"}
            </span>
            <Show
              when={isMonitor()}
              fallback={<span class="text-sm text-foreground">{p().label}</span>}
            >
              <code class="rounded bg-muted px-1.5 py-1 font-mono text-[0.72rem] text-foreground/90 wrap-break-word">
                {p().label}
              </code>
            </Show>
          </div>

          <Show when={p().summary}>
            <div class="flex flex-col gap-0.5">
              <span class="text-[0.62rem] uppercase tracking-[0.04em] text-muted-foreground">
                Latest
              </span>
              <span class="text-[0.78rem] text-foreground/90 wrap-break-word">{p().summary}</span>
            </div>
          </Show>

          <div class="flex flex-wrap gap-x-4 gap-y-0.5 text-[0.68rem] text-muted-foreground">
            <span>Started {formatRelativeTime(p().started_ts)}</span>
            <span>Updated {formatRelativeTime(p().last_event_ts)}</span>
          </div>

          {/* "Ask to stop" — subagent, while running. Routes the interrupt
              through the Agent (composer pre-fill), never a direct host kill. */}
          <Show when={isSubagent() && running()}>
            <button
              type="button"
              class="mt-0.5 inline-flex w-fit items-center gap-1.5 rounded-md border border-border px-2 py-1 text-[0.72rem] font-medium text-foreground transition-colors hover:border-status-danger/60 hover:text-status-danger"
              onClick={() => props.onAskToStop(p())}
            >
              <OctagonX class="size-3.5" aria-hidden="true" />
              Ask to stop
            </button>
          </Show>
        </div>
      </Show>
    </Card>
  );
}
