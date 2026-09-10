import { For } from "solid-js";
import { isMarkdownArtifact } from "./markdown";
import type { Artifact } from "./types";

export type ArtifactPresentationMode = "rendered" | "source";

/** Formats with a meaningful rendered representation and an inspectable source. */
export function hasArtifactPresentationModes(artifact: Artifact): boolean {
  const mime = artifact.mime.split(";", 1)[0].trim().toLowerCase();
  return artifact.kind === "solid"
    || artifact.kind === "html"
    || isMarkdownArtifact(artifact)
    || mime === "image/svg+xml";
}

const modes = ["rendered", "source"] as const;

/** Shared, keyboard-native presentation control for preview and showcase panes. */
export function ArtifactPresentationToggle(props: {
  mode: ArtifactPresentationMode;
  onChange: (mode: ArtifactPresentationMode) => void;
}) {
  return <div
    role="group"
    aria-label="Artifact presentation"
    data-slot="artifact-presentation-toggle"
    class="inline-flex shrink-0 rounded-lg border border-border bg-muted/70 p-0.5"
  >
    <For each={modes}>{mode => <button
      type="button"
      aria-pressed={props.mode === mode ? "true" : "false"}
      data-presentation-mode={mode}
      class="min-h-11 rounded-md px-2.5 text-xs font-medium text-muted-foreground transition-colors hover:text-foreground focus-visible:z-10 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring aria-pressed:bg-background aria-pressed:text-foreground aria-pressed:shadow-sm"
      onClick={() => props.onChange(mode)}
    >{mode === "rendered" ? "Rendered" : "Source"}</button>}</For>
  </div>;
}
