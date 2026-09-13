import { providerLabel } from "../components/settings/agent-config";
import { titleCase } from "../components/settings/prefs";
import { state } from "../store/store";
import type { ThreadExecutionTarget } from "./types";

/** What the "Runs on" row — and the run card header — says a turn ran on, for
 * each shape the Host sends. `muted` marks the inherited default, which is a
 * fact about the installation rather than a choice made for this Thread. */
export function executionLabel(execution: ThreadExecutionTarget | null | undefined): { text: string; muted: boolean } {
  if (!execution) {
    const fallback = [providerLabel(state.model?.provider_id), state.model?.current.id].filter(Boolean).join(" · ");
    return { text: fallback ? `Default Native · ${fallback}` : "Default Native", muted: true };
  }
  if (execution.kind === "native") return { text: ["Native", providerLabel(execution.provider_id) || execution.provider_id, execution.model].join(" · "), muted: false };
  const group = state.subagentModels?.providers.find(provider => provider.provider === execution.agent);
  return { text: [group?.label ?? titleCase(execution.agent), execution.model, titleCase(execution.variant)].join(" · "), muted: false };
}
