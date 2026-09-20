import { describe, expect, it } from "vitest";
import { ARTIFACT_CSP, ARTIFACT_SANDBOX, artifactDocument } from "./document";
import { compileArtifact } from "./compiler";
import type { Artifact } from "./types";
const artifact: Artifact = { id: 1, revision: 1, title: "Counter", kind: "solid", content: "", thread_ids: [3], created_at: "2026-09-09", updated_at: "2026-09-09" };
describe("artifact isolation", () => {
  it("allows local scripts without granting origin, forms, popups or a network", () => {
    expect(ARTIFACT_SANDBOX).toBe("allow-scripts");
    expect(ARTIFACT_CSP).toContain("connect-src 'none'");
    expect(ARTIFACT_CSP).toContain("frame-src 'none'");
    expect(ARTIFACT_CSP).toContain("form-action 'none'");
    expect(ARTIFACT_CSP).toContain("default-src 'none'");
  });
  it("compiles real Solid JSX using the Solid2 renderer", () => {
    const code = compileArtifact('import {createSignal} from "solid-js"; export default function App(){const [count,setCount]=createSignal(0);return <button onClick={()=>setCount(count()+1)}>{count()}</button>}');
    expect(code).toContain('require("@solidjs/web")');
    expect(code).toContain('require("solid-js")');
    expect(code).not.toContain("React");
    expect(artifactDocument(artifact, code)).toContain("__artifactModules");
  });
  it("keeps untrusted source from escaping the inline script element", () => {
    const document = artifactDocument(artifact, 'exports.default=()=>"</script><img src=https://example.test>";');
    expect(document).toContain('<\\/script>');
    expect(document.match(/<\/script>/g)).toHaveLength(1);
  });
  it("prepares safe recovery controls for a valid component that throws at runtime", () => {
    const code = compileArtifact('export default function App(){throw new Error("Runtime failed")}');
    const doc = new DOMParser().parseFromString(artifactDocument({ ...artifact, title: '<img src=x onerror="bad()">' }, code), "text/html");
    const panel = doc.querySelector<HTMLElement>("#artifact-error")!;
    expect(panel.hidden).toBe(true);
    expect(panel.textContent).toContain("This artifact stopped working.");
    expect(panel.textContent).toContain('ask Hirsel to repair artifact #1, “<img src=x onerror="bad()">”');
    expect(panel.querySelector("img")).toBeNull();
    // The failure reason is the line itself; nothing is folded away from it.
    expect(panel.querySelector("details")).toBeNull();
    expect(panel.querySelector("#artifact-details")).not.toBeNull();
    expect(panel.querySelector("button")?.textContent).toBe("Close preview");
    expect(code).toContain("Runtime failed");
  });
  it("escapes file content, including HTML masquerading as a file", () => {
    const document = artifactDocument({ ...artifact, kind: "file", mime: "text/plain", filename: null, content: '<script>alert("x")</script>' });
    expect(document).toContain("&lt;script&gt;");
    expect(document).not.toContain('<script>alert("x")</script>');
    expect(document).toContain("event.key === 'Escape'");
  });
  it("renders image artifacts as encoded image data with safe accessible text", () => {
    const content = '<svg xmlns="http://www.w3.org/2000/svg"><text>Cat & moon</text></svg>';
    const title = 'Cat <picture> "night"';
    const page = new DOMParser().parseFromString(artifactDocument({
      ...artifact,
      kind: "image",
      mime: "image/svg+xml",
      title,
      content,
    }), "text/html");
    const image = page.querySelector<HTMLImageElement>("img.artifact-image");
    expect(image?.alt).toBe(title);
    expect(image?.src).toBe(`data:image/svg+xml;charset=utf-8,${encodeURIComponent(content)}`);
    expect(image?.getAttribute("style")).toContain("object-fit:contain");
    expect(page.querySelector("svg")).toBeNull();
    expect(page.querySelectorAll("script")).toHaveLength(1);
  });
  it("renders raster image bytes without treating them as text", () => {
    const bytes = "iVBORw0KGgo=";
    const page = new DOMParser().parseFromString(artifactDocument({
      ...artifact,
      kind: "image",
      mime: "image/png",
      content: bytes,
    }), "text/html");
    expect(page.querySelector<HTMLImageElement>("img.artifact-image")?.src).toBe(`data:image/png;base64,${bytes}`);
    expect(page.querySelector("body > pre")).toBeNull();
  });
  it("keeps a file that only looks like markup preformatted", () => {
    const page = new DOMParser().parseFromString(artifactDocument({
      ...artifact,
      kind: "file",
      mime: "text/plain",
      filename: "cat.svg",
      content: "<svg>plain source</svg>",
    }), "text/html");
    expect(page.querySelector("img")).toBeNull();
    expect(page.querySelector("body > pre")?.textContent).toBe("<svg>plain source</svg>");
  });
});
