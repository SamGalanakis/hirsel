import { createEffect, createSignal, onCleanup, Show } from "solid-js";
import { ARTIFACT_DISMISS_MESSAGE, type Artifact } from "./types";

/** Opaque execution surface. The only accepted message dismisses this preview. */
export function ArtifactPreview(props: { artifact: Artifact; onDismiss?: () => void; onReturnToComposer?: () => void }) {
  const [document, setDocument] = createSignal("");
  const [error, setError] = createSignal<string | null>(null);
  const [attempt, setAttempt] = createSignal(0);
  let frame: HTMLIFrameElement | undefined;
  const dismissFromFrame = (event: MessageEvent) => {
    if (frame && event.source === frame.contentWindow && event.data === ARTIFACT_DISMISS_MESSAGE) props.onDismiss?.();
  };
  window.addEventListener("message", dismissFromFrame);
  onCleanup(() => window.removeEventListener("message", dismissFromFrame));
  let worker: Worker | undefined;
  let timeout: ReturnType<typeof setTimeout> | undefined;
  let generation = 0;
  const dispose = () => { worker?.terminate(); worker = undefined; clearTimeout(timeout); };
  onCleanup(() => { generation++; dispose(); });
  createEffect(() => ({ artifact: { ...props.artifact }, attempt: attempt() }), ({ artifact }) => {
    const current = ++generation;
    dispose(); setDocument(""); setError(null);
    void import("./document").then(({ artifactDocument }) => {
      if (current !== generation) return;
      if (artifact.kind !== "solid") { setDocument(artifactDocument(artifact)); return; }
      worker = new Worker(new URL("./compiler.worker.ts", import.meta.url), { type: "module" });
      timeout = setTimeout(() => { if (current === generation) { dispose(); setError("The preview took too long to compile. Ask Hirsel to simplify it."); } }, 15_000);
      worker.onmessage = (event: MessageEvent<{ code?: string; error?: string }>) => {
        if (current !== generation) return;
        dispose();
        if (event.data.error) setError(event.data.error);
        else {
          try { setDocument(artifactDocument(artifact, event.data.code)); }
          catch (cause) { setError(String(cause)); }
        }
      };
      worker.onerror = event => { if (current === generation) { dispose(); setError(`The preview compiler failed. Reopen the artifact to try again. ${event.message.slice(0, 300)}`); } };
      worker.postMessage(artifact.content);
    }).catch(cause => { if (current === generation) setError(String(cause)); });
  });
  return <Show when={!error()} fallback={<div role="alert" class="space-y-4 p-6 text-sm">
    <p class="font-medium">This artifact couldn’t be displayed.</p>
    <p class="max-w-prose leading-relaxed text-muted-foreground">Try the preview again. If it still fails, ask Hirsel to repair artifact #{props.artifact.id}, “{props.artifact.title}”. Your conversation and draft are kept.</p>
    <div class="flex flex-wrap gap-2"><button class="min-h-11 rounded-lg bg-muted px-3 font-medium focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring" onClick={() => setAttempt(value => value + 1)}>Try preview again</button><Show when={props.onReturnToComposer}><button class="min-h-11 rounded-lg px-3 text-muted-foreground hover:bg-muted focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring" onClick={props.onReturnToComposer}>Return to composer</button></Show></div>
    <details><summary class="min-h-11 cursor-pointer py-3 text-muted-foreground">Technical details</summary><pre class="max-h-64 overflow-auto whitespace-pre-wrap break-words text-xs">{error()}</pre></details>
  </div>}>
    <Show when={document()} fallback={<p role="status" class="p-6 text-sm text-muted-foreground">Preparing preview…</p>}>
      <iframe ref={node => { frame = node; }} title={props.artifact.title} srcdoc={document()} sandbox="allow-scripts" referrerpolicy="no-referrer" class="h-full min-h-64 w-full border-0 bg-white" />
    </Show>
  </Show>;
}
