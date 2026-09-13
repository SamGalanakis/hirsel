import { Activity } from "@/components/ui/icons";
import { createMemo, For, Show } from "solid-js";

import { partitionProcesses, scopedProcesses } from "../../store/selectors";
import { state } from "../../store/store";
import { historyId } from "../../lib/history";
import { threadState } from "../../threads/store";
import { getClient } from "../../ws/client";
import type { ProcessInfo } from "../../protocol";
import { Empty, EmptyDescription, EmptyHeader, EmptyMedia, EmptyTitle } from "../ui/empty";
import { ProcessRow } from "./ProcessRow";

export function ProcessesView() {
  const groups = createMemo(() => partitionProcesses(scopedProcesses(state.processes, threadState.threads, threadState.focusedId)));
  const cancel = (process: ProcessInfo) => {
    const history = historyId();
    if (history && process.active_process_id) getClient()?.cancelProcess(history, process.thread_id, process.active_process_id);
  };
  const disable = (process: ProcessInfo) => {
    const history = historyId();
    if (history && process.trigger_subscription_key && process.trigger_revision !== null) {
      getClient()?.disableProcessTrigger(history, process.thread_id, process.trigger_subscription_key, process.trigger_revision);
    }
  };

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
              <EmptyTitle>No processes</EmptyTitle>
              <EmptyDescription>
                Lash processes for this Thread and its descendants will appear here.
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
              {(process) => <ProcessRow process={process} onCancel={cancel} onDisableTrigger={disable} />}
            </For>
          </section>
        </Show>

        <Show when={groups().finished.length > 0}>
          <section class="flex flex-col gap-3">
            <h2 class="mx-3 text-xs font-medium text-muted-foreground">
              Finished ({groups().finished.length})
            </h2>
            <For each={groups().finished}>
              {(process) => <ProcessRow process={process} onCancel={cancel} onDisableTrigger={disable} />}
            </For>
          </section>
        </Show>
      </div>
    </Show>
  );
}
