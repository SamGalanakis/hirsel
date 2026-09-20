import { createMemo, For, Show } from "solid-js";
import { openArtifact } from "../artifacts/store";
import { artifactState } from "../artifacts/store";
import { SectionLabel } from "../components/ui/section-label";
import { openThreadReach } from "../grants/reach";
import { focusThread, threadState } from "../threads/store";
import type { ThreadActivity } from "../threads/types";
import type { TimelineEvent } from "../store/types";

type Target = { kind: "thread"; id: number } | { kind: "artifact"; id: number } | { kind: "root" };
interface Pill { key: string; verb: "Created" | "Sent to" | "Delegated" | "Read" | "Edited" | "Refused"; target: Target; refusal?: { reason: string; sourceThreadId: number } }
const actionClass = "inline-flex min-h-11 items-center rounded-full border border-border bg-background px-3 text-xs font-medium text-foreground hover:bg-muted focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring";
function object(payload: { text: string; truncated: boolean } | null | undefined): Record<string, unknown> | null {
  if (!payload || payload.truncated) return null;
  try { const value: unknown = JSON.parse(payload.text); return value !== null && typeof value === "object" && !Array.isArray(value) ? value as Record<string, unknown> : null; }
  catch { return null; }
}
function positive(value: unknown): number | null { return typeof value === "number" && Number.isSafeInteger(value) && value > 0 ? value : null; }
function toolPills(events: TimelineEvent[]): Pill[] {
  const starts = new Map(events.filter(row => row.event.kind === "tool_start").map(row => [row.event.kind === "tool_start" ? row.event.id : "", row.event]));
  const pills: Pill[] = [];
  for (const row of events) {
    if (row.event.kind !== "tool_done" || !row.event.ok) continue;
    const start = starts.get(row.event.id); if (!start || start.kind !== "tool_start") continue;
    const input = object(start.input), result = object(row.event.result); if (!input || !result) continue;
    const threadId = positive(result.thread_id) ?? positive(input.thread);
    const artifactId = positive(result.artifact_id) ?? positive(input.artifact_id) ?? (start.name === "artifacts_create" ? positive(result.id) : null);
    const mapping: Record<string, Pill["verb"] | undefined> = { threads_create: "Created", threads_send: "Sent to", threads_delegate: "Delegated", threads_read: "Read", threads_update: "Edited", threads_state: "Edited", artifacts_create: "Created", artifacts_edit: "Edited", artifacts_show: "Read" };
    const verb = mapping[start.name]; if (!verb) continue;
    const target: Target | null = threadId ? { kind: "thread", id: threadId } : artifactId ? { kind: "artifact", id: artifactId } : null;
    if (target) pills.push({ key: row.event.id, verb, target });
  }
  return pills;
}
function refusalPills(activities: ThreadActivity[], sourceThreadId: number): Pill[] {
  return activities.filter(activity => activity.kind === "refusal").flatMap(activity => {
    const data = activity.data as Record<string, unknown> | null; const target = data?.target as Record<string, unknown> | undefined;
    const reason = typeof data?.reason === "string" ? data.reason : null;
    const parsed: Target | null = target?.kind === "thread" && positive(target.thread_id) ? { kind: "thread", id: positive(target.thread_id)! }
      : target?.kind === "artifact" && positive(target.artifact_id) ? { kind: "artifact", id: positive(target.artifact_id)! }
      : target?.kind === "root" ? { kind: "root" } : null;
    return reason && parsed ? [{ key: `refusal-${activity.id}`, verb: "Refused" as const, target: parsed, refusal: { reason, sourceThreadId } }] : [];
  });
}
function name(target: Target): string {
  if (target.kind === "thread") return threadState.threads.find(thread => thread.id === target.id)?.title ?? `Thread #${target.id}`;
  if (target.kind === "artifact") return artifactState.summaries.find(artifact => artifact.id === target.id)?.title ?? `Artifact ${target.id}`;
  return "Everything";
}
function open(target: Target) { if (target.kind === "thread") focusThread(target.id); else if (target.kind === "artifact") openArtifact(target.id); }
export function EffectPills(props: { turnId: number; threadId: number; activities: ThreadActivity[]; events: TimelineEvent[] }) {
  const pills = createMemo(() => [...toolPills(props.events), ...refusalPills(props.activities.filter(activity => activity.turn_id === props.turnId), props.threadId)]);
  return <Show when={pills().length}><section class="mt-2" data-slot="effect-pills" data-turn-id={props.turnId} aria-label="Touched by this reply"><SectionLabel class="mb-1.5">Touched</SectionLabel><div class="flex flex-wrap gap-1.5"><For each={pills()}>{pill => <div class="flex min-w-0 flex-wrap items-center gap-1.5 rounded-xl border border-border/60 bg-muted/20 p-1.5" data-effect={pill.verb.toLowerCase().replace(" ", "_")}><span class="max-w-64 truncate px-1.5 text-xs">{pill.verb} · {name(pill.target)}</span><Show when={pill.target.kind !== "root"}><button type="button" class={actionClass} onClick={() => open(pill.target)}>Open</button></Show><Show when={pill.refusal}>{refusal => <div class="basis-full pl-1 text-meta text-muted-foreground"><Show when={refusal().reason === "owner_fence"} fallback={<Show when={pill.target.kind === "artifact"} fallback={<><p>Reach would cover this Thread and its subtree. Granting never retries the refused operation.</p><button type="button" class={`${actionClass} mt-2`} onClick={() => { const source = threadState.threads.find(thread => thread.id === refusal().sourceThreadId); if (source) openThreadReach(source); }}>Review reach</button></>}><p>Artifacts have no owning Space to guess. Share an explicit artifact reference instead.</p></Show>}><p>This is an ancestor fence. A subtree grant cannot open it; only existing root reach can.</p></Show></div>}</Show></div>}</For></div></section></Show>;
}
