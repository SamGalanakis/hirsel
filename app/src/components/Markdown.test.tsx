import { render, waitFor } from "@solidjs/testing-library";
import { describe, expect, it } from "vitest";
import { Markdown, renderInline, stripInlineMarkdown } from "./Markdown";

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

  it("strips heading and list syntax the old regex pass left behind", () => {
    expect(stripInlineMarkdown("### Reading the socket")).toBe("Reading the socket");
    expect(stripInlineMarkdown("~~dropped~~ plan")).toBe("dropped plan");
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

describe("Markdown blocks", () => {
  it("renders the reported failure sample: ### heading + GFM table + bold + inline code", () => {
    const source = [
      "### Socket status",
      "",
      "The **relay** is up; run `just dev` to attach.",
      "",
      "| Host | State |",
      "| --- | ----: |",
      "| alpha | `ok` |",
      "| beta | down |",
    ].join("\n");
    const { container } = render(() => <Markdown>{source}</Markdown>);

    const heading = container.querySelector("h3");
    expect(heading?.textContent).toBe("Socket status");
    expect(container.textContent).not.toContain("###");

    const table = container.querySelector("table");
    expect(table).toBeTruthy();
    expect(container.querySelectorAll("th")).toHaveLength(2);
    expect(container.querySelectorAll("tbody tr")).toHaveLength(2);
    expect(container.textContent).not.toContain("| Host |");

    expect(container.querySelector("strong")?.textContent).toBe("relay");
    expect(container.querySelector("p code")?.textContent).toBe("just dev");
  });

  it("clamps deep headings to the h3 scale while keeping their level", () => {
    const { container } = render(() => <Markdown>{"##### deep"}</Markdown>);
    const h5 = container.querySelector("h5");
    expect(h5?.textContent).toBe("deep");
    expect(h5?.getAttribute("class")).toContain("text-sm");
  });

  it("scrolls wide tables inside their own container instead of widening the column", () => {
    const { container } = render(() => (
      <Markdown>{"| a | b |\n| - | - |\n| 1 | 2 |"}</Markdown>
    ));
    const wrapper = container.querySelector("table")?.parentElement;
    expect(wrapper?.getAttribute("class")).toContain("overflow-x-auto");
  });

  it("renders ordered, nested, and task lists", () => {
    const source = [
      "1. first",
      "2. second",
      "   - nested one",
      "   - nested two",
      "",
      "- [x] shipped",
      "- [ ] pending",
    ].join("\n");
    const { container } = render(() => <Markdown>{source}</Markdown>);

    const ol = container.querySelector("ol");
    expect(ol).toBeTruthy();
    expect(ol?.querySelectorAll(":scope > li")).toHaveLength(2);
    expect(ol?.querySelector("ul li")?.textContent).toBe("nested one");

    const boxes = container.querySelectorAll<HTMLInputElement>("input[type=checkbox]");
    expect(boxes).toHaveLength(2);
    expect(boxes[0].checked).toBe(true);
    expect(boxes[1].checked).toBe(false);
    expect([...boxes].every((box) => box.disabled)).toBe(true);
    expect(container.textContent).not.toContain("[x]");
  });

  it("renders blockquotes, strikethrough, and thematic breaks", () => {
    const { container } = render(() => (
      <Markdown>{"> quoted line\n\n~~gone~~\n\n---\n\ntail"}</Markdown>
    ));
    expect(container.querySelector("blockquote")?.textContent).toContain("quoted line");
    expect(container.querySelector("del")?.textContent).toBe("gone");
    expect(container.querySelector("hr")).toBeTruthy();
    expect(container.textContent).not.toContain("~~");
  });

  it("opens links safely and keeps non-http schemes as inert text", () => {
    const { container } = render(() => (
      <Markdown>{"[docs](https://x.dev/y) and [bad](javascript:alert(1))"}</Markdown>
    ));
    const links = container.querySelectorAll("a");
    expect(links).toHaveLength(1);
    expect(links[0].getAttribute("href")).toBe("https://x.dev/y");
    expect(links[0].getAttribute("target")).toBe("_blank");
    expect(links[0].getAttribute("rel")).toBe("noopener noreferrer nofollow");
    expect(container.textContent).toContain("bad");
  });

  it("renders code fences as plain mono text before the highlighter loads", () => {
    const { container } = render(() => (
      <Markdown>{"```rust\nfn main() {}\n```"}</Markdown>
    ));
    const code = container.querySelector("pre code");
    expect(code?.textContent).toBe("fn main() {}");
    expect(container.textContent).toContain("rust");
    expect(container.textContent).not.toContain("```");
  });

  it("upgrades a code fence in place once the lazy highlighter resolves", async () => {
    const { container } = render(() => <Markdown>{"```rust\nfn main() {}\n```"}</Markdown>);
    await waitFor(() => expect(container.querySelector("pre code span")).toBeTruthy());
    expect(container.querySelector("pre code")?.textContent).toBe("fn main() {}");
    expect(container.querySelector("pre code span")?.getAttribute("class")).toContain("hljs-");
  });

  it("constrains images and gives them lazy loading", () => {
    const { container } = render(() => <Markdown>{"![shot](https://x.dev/a.png)"}</Markdown>);
    const img = container.querySelector("img");
    expect(img?.getAttribute("loading")).toBe("lazy");
    expect(img?.getAttribute("class")).toContain("max-w-full");
  });
});

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

  it("does not crash on a half-streamed fence and still renders the code", () => {
    const { container } = render(() => <Markdown>{"intro\n\n```ts\nconst a = 1"}</Markdown>);
    expect(container.querySelector("pre code")?.textContent).toContain("const a = 1");
    expect(container.textContent).toContain("intro");
  });

  it("styles an unterminated bold run instead of showing literal asterisks", () => {
    const { container } = render(() => <Markdown>{"the **relay is"}</Markdown>);
    expect(container.querySelector("strong")?.textContent).toBe("relay is");
    expect(container.textContent).not.toContain("**");
  });

  it("renders a half-typed link as its label, never a broken href", () => {
    const { container } = render(() => <Markdown>{"see [the docs](https://x.d"}</Markdown>);
    expect(container.querySelectorAll("a")).toHaveLength(0);
    expect(container.textContent).toContain("the docs");
  });
});
