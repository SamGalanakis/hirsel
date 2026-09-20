import { For, Show } from "solid-js";
import { ArtifactCard } from "../artifacts/ArtifactSurface";
import { SectionLabel } from "../components/ui/section-label";
import type { ThreadState } from "./types";

export function TaskState(props: { state: ThreadState }) {
  return <section data-slot="task-state" aria-labelledby="task-state-heading" class="rounded-xl border border-border/60 bg-muted/20 p-4">
    <SectionLabel as="h2" id="task-state-heading">Task state</SectionLabel>
    <p class="mt-2 text-base font-medium text-foreground" data-slot="task-headline">{props.state.headline}</p>
    <Show when={props.state.findings.length > 0}>
      <ul class="mt-3 list-disc space-y-1 pl-5 text-sm text-muted-foreground">
        <For each={props.state.findings}>{finding => <li>{finding}</li>}</For>
      </ul>
    </Show>
    <Show when={props.state.artifact_ids.length > 0}>
      <div class="mt-3 grid gap-2">
        <For each={props.state.artifact_ids}>{id => <ArtifactCard id={id} />}</For>
      </div>
    </Show>
    <p class="mt-3 text-meta tabular-nums text-muted-foreground">State revision {props.state.revision}</p>
  </section>;
}
