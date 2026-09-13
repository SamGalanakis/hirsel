/** An `openui` artifact on a real surface.
 *
 * The drawing is native, in the host document, with the app's own tokens — no
 * iframe, because there is nothing to execute: the body is data. What an
 * interaction produces is an ordinary Owner message in the artifact's Thread,
 * so the conversation stays the single record of what was asked. */
import { createSignal, Show } from "solid-js";
import { historyId } from "../lib/history";
import { sendThreadMessage, threadState } from "../threads/store";
import type { Artifact } from "../artifacts/types";
import type { OpenUiAction } from "./context";
import { Renderer } from "./Renderer";

/** The message an action sends: what a person would have written, then the
 * exact payload the agent needs, fenced so it survives the round trip. */
export function actionMessageBody(artifactId: number, action: OpenUiAction): string {
  const payload = {
    artifact_id: artifactId,
    action: action.action ?? "submit",
    params: action.params,
    form_state: action.formState ?? {},
  };
  return `${action.message}\n\n\`\`\`json\n${JSON.stringify(payload, null, 2)}\n\`\`\``;
}

export function OpenUiArtifact(props: { artifact: Artifact; threadId?: number | null }) {
  const [error, setError] = createSignal<string | null>(null);
  const target = () => {
    const id = props.threadId ?? threadState.focusedId;
    return id !== null && id !== undefined && threadState.threads.some(thread => thread.id === id) ? id : null;
  };
  const send = (action: OpenUiAction) => {
    const threadId = target();
    const history = historyId();
    if (threadId === null || !history) { setError("Open this artifact from a Thread to use its controls."); return; }
    try {
      sendThreadMessage(history, threadId, actionMessageBody(props.artifact.id, action), "send", [], [], [props.artifact.id]);
      setError(null);
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : "Couldn’t send that to the Thread.");
    }
  };
  return <div data-slot="openui-artifact" class="min-h-full p-4 text-foreground">
    <Renderer body={props.artifact.content} onAction={send} />
    <Show when={error()}><p role="alert" class="mt-3 text-meta text-status-danger">{error()}</p></Show>
  </div>;
}
