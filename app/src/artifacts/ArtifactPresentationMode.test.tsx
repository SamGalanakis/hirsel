import { fireEvent, render } from "@solidjs/testing-library";
import { createSignal } from "solid-js";
import { describe, expect, it } from "vitest";
import { ArtifactPresentationToggle, hasArtifactPresentationModes, type ArtifactPresentationMode } from "./ArtifactPresentationMode";
import type { Artifact } from "./types";

const artifact: Artifact = { id: 1, title: "Artifact", kind: "file", mime: "text/plain", filename: "notes.txt", content: "content", thread_ids: [], created_at: "a", updated_at: "b" };

describe("artifact presentation modes", () => {
  it("recognizes every renderable format without adding a redundant plain-text control", () => {
    expect(hasArtifactPresentationModes({ ...artifact, kind: "html", mime: "text/html" })).toBe(true);
    expect(hasArtifactPresentationModes({ ...artifact, kind: "solid", mime: "text/jsx" })).toBe(true);
    expect(hasArtifactPresentationModes({ ...artifact, mime: "text/markdown; charset=utf-8" })).toBe(true);
    expect(hasArtifactPresentationModes({ ...artifact, mime: "text/plain", filename: "README.MD" })).toBe(true);
    expect(hasArtifactPresentationModes({ ...artifact, mime: " IMAGE/SVG+XML ; charset=utf-8", filename: "shape.svg" })).toBe(true);
    expect(hasArtifactPresentationModes(artifact)).toBe(false);
  });
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
