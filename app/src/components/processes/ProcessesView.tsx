import { Activity } from "lucide-solid";
import { createMemo, For, Show } from "solid-js";
import type { ProcessInfo } from "../../protocol";
import { partitionProcesses } from "../../store/selectors";
import { closeRightRegion, prefillComposer, state } from "../../store/store";
import { snippet } from "../../lib/format";
import { Empty, EmptyDescription, EmptyHeader, EmptyMedia, EmptyTitle } from "../ui/empty";
import { ProcessRow } from "./ProcessRow";

export function ProcessesView() {
  const groups = createMemo(() => partitionProcesses(state.processes));

  // "Ask Hirsel to stop": interrupts route through globally aware Hirsel. The task
  // world stays put while the standing composer receives the request.
  function handleAskToStop(process: ProcessInfo) {
    closeRightRegion();
    prefillComposer(`stop process ${process.id} (${snippet(process.label, 48)})`);
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
                Sub-agents and monitors Hirsel starts will show up here.
              </EmptyDescription>
            </EmptyHeader>
          </Empty>
        </div>
      }
    >
      <div class="flex flex-1 flex-col gap-3 overflow-y-auto py-3 pb-6">
        <Show when={groups().running.length > 0}>
          <section class="flex flex-col gap-3">
            <h2 class="mx-3 text-xs font-medium text-muted-foreground">
              Running ({groups().running.length})
            </h2>
            <For each={groups().running}>
              {(process) => <ProcessRow process={process} onAskToStop={handleAskToStop} />}
            </For>
          </section>
        </Show>

        <Show when={groups().finished.length > 0}>
          <section class="flex flex-col gap-3">
            <h2 class="mx-3 text-xs font-medium text-muted-foreground">
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
