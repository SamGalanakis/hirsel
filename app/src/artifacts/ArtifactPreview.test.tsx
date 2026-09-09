import { fireEvent, render, waitFor } from "@solidjs/testing-library";
import { afterEach, describe, expect, it, vi } from "vitest";
import { ArtifactPreview } from "./ArtifactPreview";
import { ARTIFACT_DISMISS_MESSAGE, type Artifact } from "./types";

vi.mock("./document", () => ({ artifactDocument: () => "<!doctype html><button>Preview</button>" }));
const artifact: Artifact = { id: 4, title: "Preview", kind: "html", mime: "text/html", content: "<button>Preview</button>", thread_ids: [5], created_at: "a", updated_at: "b" };

afterEach(() => vi.unstubAllGlobals());
describe("artifact preview recovery", () => {
  it("offers retry and return to the composer while keeping diagnostics disclosed", async () => {
    const workers: FakeWorker[] = [];
    class FakeWorker {
      onmessage: ((event: MessageEvent) => void) | null = null;
      postMessage = vi.fn(); terminate = vi.fn();
      constructor() { workers.push(this); }
    }
    vi.stubGlobal("Worker", FakeWorker);
    const onReturn = vi.fn();
    const view = render(() => <ArtifactPreview artifact={{ ...artifact, kind: "solid" }} onReturnToComposer={onReturn} />);
    await waitFor(() => expect(workers).toHaveLength(1));
    workers[0].onmessage?.(new MessageEvent("message", { data: { error: "SyntaxError: Unexpected token" } }));
    await waitFor(() => expect(view.getByRole("alert")).toHaveTextContent("ask Hirsel to repair artifact #4"));
    expect(view.container.querySelector("details")?.open).toBe(false);
    expect(view.getByText("SyntaxError: Unexpected token")).toBeTruthy();
    fireEvent.click(view.getByRole("button", { name: "Try preview again" }));
    await waitFor(() => expect(workers).toHaveLength(2));
    expect(workers[0].terminate).toHaveBeenCalled();
    workers[1].onmessage?.(new MessageEvent("message", { data: { error: "Still invalid" } }));
    await waitFor(() => expect(view.getByRole("button", { name: "Return to composer" })).toBeTruthy());
    fireEvent.click(view.getByRole("button", { name: "Return to composer" }));
    expect(onReturn).toHaveBeenCalledOnce();
    view.unmount();
  });
});
describe("artifact keyboard dismissal boundary", () => {
  it("accepts only the fixed dismissal signal from its own mounted opaque frame", async () => {
    const dismiss = vi.fn();
    const view = render(() => <ArtifactPreview artifact={artifact} onDismiss={dismiss} />);
    await waitFor(() => expect(view.container.querySelector("iframe")).not.toBeNull());
    const frame = view.container.querySelector("iframe")!;
    expect(frame.getAttribute("sandbox")).toBe("allow-scripts");
    const send = (source: Window | null, data: unknown) => window.dispatchEvent(new MessageEvent("message", { source, data }));
    send(window, ARTIFACT_DISMISS_MESSAGE);
    send(null, ARTIFACT_DISMISS_MESSAGE);
    send(frame.contentWindow, { type: ARTIFACT_DISMISS_MESSAGE, command: "send_message" });
    send(frame.contentWindow, "send_message");
    expect(dismiss).not.toHaveBeenCalled();
    const source = frame.contentWindow;
    send(source, ARTIFACT_DISMISS_MESSAGE);
    expect(dismiss).toHaveBeenCalledTimes(1);
    view.unmount();
    send(source, ARTIFACT_DISMISS_MESSAGE);
    expect(dismiss).toHaveBeenCalledTimes(1);
  });
});
