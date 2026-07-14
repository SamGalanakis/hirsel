import { render } from "@solidjs/testing-library";
import { describe, expect, it } from "vitest";
import { renderInline, stripInlineMarkdown } from "./Markdown";

describe("stripInlineMarkdown (shimmer-marker transform)", () => {
  it("drops **bold** markers, keeping the words (no literal asterisks)", () => {
    const out = stripInlineMarkdown("**Inspecting pings resolve behavior**");
    expect(out).toBe("Inspecting pings resolve behavior");
    expect(out).not.toContain("*");
  });

  it("drops `code`, *em*, and [link](url) syntax to plain text", () => {
    expect(stripInlineMarkdown("running `grep` now")).toBe("running grep now");
    expect(stripInlineMarkdown("a *subtle* nudge")).toBe("a subtle nudge");
    expect(stripInlineMarkdown("see [docs](https://x.dev/y)")).toBe("see docs");
  });

  it("leaves plain text untouched", () => {
    expect(stripInlineMarkdown("Thinking…")).toBe("Thinking…");
  });
});

describe("renderInline (reasoning / inline nodes)", () => {
  it("renders **bold** as a <strong> with no literal asterisks in the output", () => {
    const { container } = render(() => <p>{renderInline("**Inspecting** pings")}</p>);
    expect(container.querySelector("strong")?.textContent).toBe("Inspecting");
    expect(container.textContent).not.toContain("*");
    expect(container.textContent).toBe("Inspecting pings");
  });

  it("mono-izes `code` and keeps its text", () => {
    const { container } = render(() => <p>{renderInline("run `grep` here")}</p>);
    expect(container.querySelector("code")?.textContent).toBe("grep");
    expect(container.textContent).not.toContain("`");
  });
});
