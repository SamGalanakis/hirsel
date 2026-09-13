import { providerLabel } from "../components/settings/agent-config";
import { titleCase } from "../components/settings/prefs";
import { state } from "../store/store";
import type { ThreadExecutionTarget } from "./types";

/** What the "Runs on" row — and the run card header — says a turn ran on, for
 * each shape the Host sends. `muted` marks the inherited default, which is a
 * fact about the installation rather than a choice made for this Thread. */
export function executionLabel(execution: ThreadExecutionTarget | null | undefined): { text: string; muted: boolean } {
  if (!execution) {
    const coordinator = [providerLabel(state.model?.provider_id), state.model?.current.id].filter(Boolean).join(" · ");
    return { text: coordinator ? `Default coordinator · ${coordinator}` : "Default coordinator", muted: true };
  }
  if (execution.kind === "host") return { text: ["Coordinator", providerLabel(execution.provider_id) || execution.provider_id, execution.model].join(" · "), muted: false };
  if (execution.kind === "lash") {
    const worker = state.subagentModels?.native_worker.label ?? "Native worker";
    return { text: [worker, providerLabel(execution.provider_id) || execution.provider_id, execution.model, titleCase(execution.variant)].join(" · "), muted: false };
  }
  const group = state.subagentModels?.providers.find(provider => provider.provider === execution.agent);
  return { text: [group?.label ?? titleCase(execution.agent), execution.model, titleCase(execution.variant)].join(" · "), muted: false };
}
