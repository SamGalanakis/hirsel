import { Activity } from "lucide-solid";
import { createMemo, For, Show } from "solid-js";
import type { ProcessInfo } from "../../protocol";
import { partitionProcesses } from "../../store/selectors";
import { goToChat, state } from "../../store/store";
import { Empty, EmptyDescription, EmptyHeader, EmptyMedia, EmptyTitle } from "../ui/empty";
import { ProcessRow } from "./ProcessRow";

/** Short prompt/label snippet for the "Ask to stop" composer pre-fill. */
function snippet(label: string): string {
  const oneLine = label.replace(/\s+/g, " ").trim();
  return oneLine.length > 48 ? `${oneLine.slice(0, 48)}…` : oneLine;
}

export function ProcessesView() {
  const groups = createMemo(() => partitionProcesses(state.processes));

  // "Ask to stop": interrupts route through the Agent by design — switch to
  // Chat with the composer pre-filled so the Owner sends the request.
  function handleAskToStop(process: ProcessInfo) {
    goToChat({ composerPrefill: `stop process ${process.id} (${snippet(process.label)})` });
  }

  return (
    <Show
      when={groups().running.length > 0 || groups().finished.length > 0}
      fallback={
        <div class="flex flex-1 flex-col p-3">
          <Empty class="border-none">
            <EmptyHeader>
              <EmptyMedia variant="icon">
                <Activity />
              </EmptyMedia>
              <EmptyTitle>Nothing running</EmptyTitle>
              <EmptyDescription>
                Sub-agents and monitors the Agent starts will show up here.
              </EmptyDescription>
            </EmptyHeader>
          </Empty>
        </div>
      }
    >
      <div class="thin-scrollbar flex flex-1 flex-col gap-3 overflow-y-auto py-3 pb-6">
        <Show when={groups().running.length > 0}>
          <section class="flex flex-col gap-3">
            <h2 class="mx-3 text-[0.68rem] font-semibold uppercase tracking-[0.06em] text-muted-foreground">
              Running ({groups().running.length})
            </h2>
            <For each={groups().running}>
              {(process) => <ProcessRow process={process} onAskToStop={handleAskToStop} />}
            </For>
          </section>
        </Show>

        <Show when={groups().finished.length > 0}>
          <section class="flex flex-col gap-3">
            <h2 class="mx-3 text-[0.68rem] font-semibold uppercase tracking-[0.06em] text-muted-foreground">
              Finished ({groups().finished.length})
            </h2>
            <For each={groups().finished}>
              {(process) => <ProcessRow process={process} onAskToStop={handleAskToStop} />}
            </For>
          </section>
        </Show>
      </div>
    </Show>
  );
}
