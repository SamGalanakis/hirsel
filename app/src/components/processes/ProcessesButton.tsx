import { Activity } from "lucide-solid";
import { Show } from "solid-js";
import { runningProcessCount } from "../../store/selectors";
import { openProcesses, state } from "../../store/store";

/** Header icon replacing the old Processes tab (spec [P1]: the bottom TabBar
 * is gone, Chat is the whole app). Sits next to `ConnectionPill`; carries the
 * same `runningProcessCount` badge in `status-active` tone the tab used to.
 * Tapping selects the Processes right-region pane (a full-screen sheet on phone). */
export function ProcessesButton() {
  const count = () => runningProcessCount(state.processes);
  const label = () => (count() > 99 ? "99+" : String(count()));

  return (
    <button
      type="button"
      class="relative flex items-center rounded-full p-1.5 text-muted-foreground transition-colors hover:text-foreground"
      onClick={openProcesses}
      aria-label="Processes"
    >
      <Activity class="size-5" aria-hidden="true" />
      <Show when={count() > 0}>
        <span class="absolute top-0 right-0 grid h-4 min-w-4 place-items-center rounded-full bg-status-active px-1 text-[0.65rem] font-bold text-primary-foreground">
          {label()}
        </span>
      </Show>
    </button>
  );
}
