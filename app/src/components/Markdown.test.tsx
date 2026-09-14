import { render } from "@solidjs/testing-library";
import { describe, expect, it } from "vitest";
import { Markdown } from "./Markdown";

describe("Markdown safety and streaming", () => {
  it("keeps raw HTML inert — no script, no img, just text", () => {
    const source = '<script>window.pwned = 1</script>\n\n<img src=x onerror="window.pwned = 1">';
    const { container } = render(() => <Markdown>{source}</Markdown>);
    expect(container.querySelector("script")).toBeNull();
    expect(container.querySelector("img")).toBeNull();
    // The markup arrives escaped as text, so no live element and no handler.
    expect(container.querySelector("[onerror]")).toBeNull();
    expect(container.innerHTML).toContain("&lt;img");
    expect(container.textContent).toContain("<script>");
    expect((window as unknown as { pwned?: number }).pwned).toBeUndefined();
  });
});
