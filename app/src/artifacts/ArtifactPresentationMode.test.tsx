import { fireEvent, render } from "@solidjs/testing-library";
import { createSignal } from "solid-js";
import { describe, expect, it } from "vitest";
import { ArtifactPresentationToggle, type ArtifactPresentationMode } from "./ArtifactPresentationMode";

describe("artifact presentation modes", () => {
  it("exposes two native pressed buttons and retains focus when changing mode", () => {
    const [mode, setMode] = createSignal<ArtifactPresentationMode>("rendered");
    const view = render(() => <ArtifactPresentationToggle mode={mode()} onChange={setMode} />);
    const rendered = view.getByRole("button", { name: "Rendered" });
    const source = view.getByRole("button", { name: "Source" });
    expect(rendered).toHaveAttribute("aria-pressed", "true");
    source.focus(); fireEvent.click(source);
    expect(source).toHaveAttribute("aria-pressed", "true");
    expect(document.activeElement).toBe(source);
    expect(rendered).toHaveAttribute("aria-pressed", "false");
  });
});
