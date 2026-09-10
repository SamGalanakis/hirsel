import { Activity } from "@/components/ui/icons";
import { createMemo, For, Show } from "solid-js";

import { partitionProcesses } from "../../store/selectors";
import { state } from "../../store/store";
import { Empty, EmptyDescription, EmptyHeader, EmptyMedia, EmptyTitle } from "../ui/empty";
import { ProcessRow } from "./ProcessRow";

export function ProcessesView() {
  const groups = createMemo(() => partitionProcesses(state.processes));

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
              <EmptyTitle>No monitors</EmptyTitle>
              <EmptyDescription>
                Monitors will appear here when the Agent creates them.
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
              {(process) => <ProcessRow process={process} />}
            </For>
          </section>
        </Show>

        <Show when={groups().finished.length > 0}>
          <section class="flex flex-col gap-3">
            <h2 class="mx-3 text-xs font-medium text-muted-foreground">
              Finished ({groups().finished.length})
            </h2>
            <For each={groups().finished}>
              {(process) => <ProcessRow process={process} />}
            </For>
          </section>
        </Show>
      </div>
    </Show>
  );
}
