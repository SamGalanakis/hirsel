import { For, Show } from "solid-js";
import { artifactState } from "../artifacts/store";
import { threadGrants } from "../grants/store";
import { SectionLabel } from "../components/ui/section-label";
import { effectErrorForTurn, effectSourceThreadId, effectsForTurn, reviewRefusedReach, runEffectAction } from "./store";
import { threadState } from "../threads/store";
import type { EffectAction, ThreadEffect, ThreadEffectTarget } from "../threads/types";

const actionClass = "inline-flex min-h-11 items-center rounded-full border border-border bg-background px-3 text-xs font-medium text-foreground transition-colors hover:bg-muted focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring disabled:opacity-40";
function targetName(target: ThreadEffectTarget): string {
  if (target.kind === "thread") return threadState.threads.find(thread => thread.id === target.thread_id)?.title ?? `Thread #${target.thread_id}`;
  if (target.kind === "artifact") return artifactState.summaries.find(artifact => artifact.id === target.artifact_id)?.title ?? `Artifact ${target.artifact_id}`;
  return "Everything";
}
function effectVerb(effect: ThreadEffect): string {
  return ({ created: "Created", sent_to: "Sent to", delegated: "Delegated", read: "Read", edited: "Edited", refused: "Refused" } as const)[effect.receipt.effect];
}
function actionLabel(action: EffectAction): string {
  return action.kind === "open" ? "Open" : action.kind === "archive" ? "Archive" : action.kind === "cancel_queued" ? "Cancel queued" : "Stop";
}
function grantHeld(sourceTurnId: number, targetId: number): boolean {
  const sourceThreadId = effectSourceThreadId(sourceTurnId);
  return sourceThreadId !== undefined && threadGrants(sourceThreadId).some(grant => grant.target.kind === "thread" && grant.target.thread_id === targetId);
}
function RefusalHelp(props: { effect: ThreadEffect }) {
  const refusal = () => props.effect.receipt.refusal;
  const target = () => props.effect.receipt.target;
  const threadTargetId = () => { const current = target(); return current.kind === "thread" ? current.thread_id : null; };
  const grantable = () => refusal()?.reason !== "owner_fence" && target().kind !== "artifact";
  return <div class="basis-full pl-1 text-meta text-muted-foreground" data-slot="effect-refusal-help">
    <Show when={refusal()?.reason === "owner_fence"} fallback={<Show when={target().kind === "artifact"} fallback={<Show when={target().kind === "thread"} fallback={<p>This attempted everything (root). Review Reach to grant or revoke access to the whole history. Granting never retries the refused operation.</p>}><>
      <p>Reach would cover this Thread and everything below it. Granting never retries the refused operation.</p>
      <Show when={threadTargetId()}>{targetId => <Show when={grantHeld(props.effect.receipt.turn_id, targetId())}><p>That subtree is now granted; Reach also lets you revoke it.</p></Show>}</Show>
    </></Show>}><p>Artifacts have no owning Space to guess. Share an explicit artifact reference instead.</p></Show>}>
      <p>This is an ancestor fence. An ordinary subtree grant cannot open it; only existing root reach can.</p>
    </Show>
    <Show when={grantable()}><button type="button" class={`${actionClass} mt-2`} onClick={() => reviewRefusedReach(props.effect.receipt.turn_id, threadTargetId() ?? undefined)}>Review reach</button></Show>
  </div>;
}

export function EffectPills(props: { turnId: number }) {
  const effects = () => effectsForTurn(props.turnId);
  return <Show when={effects().length > 0}><section class="mt-2" data-slot="effect-pills" aria-label="Touched by this reply">
    <SectionLabel class="mb-1.5">Touched</SectionLabel>
    <div class="flex flex-wrap gap-1.5">
      <For each={effects()}>{effect => <div class="flex min-w-0 flex-wrap items-center gap-1.5 rounded-xl border border-border/60 bg-muted/20 p-1.5" data-effect={effect.receipt.effect} data-effect-id={effect.receipt.id}>
        <span class="min-w-0 max-w-64 truncate px-1.5 text-xs text-foreground" title={targetName(effect.receipt.target)}>{effectVerb(effect)} · {targetName(effect.receipt.target)}</span>
        <For each={effect.actions}>{action => <button type="button" class={actionClass} onClick={() => runEffectAction(action, props.turnId)}>{actionLabel(action)}</button>}</For>
        <Show when={effect.receipt.effect === "refused"}><RefusalHelp effect={effect} /></Show>
      </div>}</For>
    </div>
    <Show when={effectErrorForTurn(props.turnId)}>{error => <p class="mt-1.5 text-meta text-destructive" role="alert">{error()}</p>}</Show>
  </section></Show>;
}
