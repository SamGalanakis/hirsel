import { describe, expect, it } from "vitest";
import { highlight, resolveLanguage } from "./highlight";

describe("resolveLanguage", () => {
  it("maps the fence info strings agents write onto carried grammars", () => {
    expect(resolveLanguage("rs")).toBe("rust");
    expect(resolveLanguage("TSX")).toBe("typescript");
    expect(resolveLanguage(" shell ")).toBe("bash");
  });

  it("returns null for unknown or missing languages so code renders plain", () => {
    expect(resolveLanguage(null)).toBeNull();
    expect(resolveLanguage("")).toBeNull();
    expect(resolveLanguage("brainfuck")).toBeNull();
  });
});

describe("highlight", () => {
  it("returns a hast tree of spans for a carried language", async () => {
    const tree = await highlight("fn main() {}", "rust");
    expect(tree).toBeTruthy();
    const classes = new Set<string>();
    const walk = (nodes: readonly { type: string; [key: string]: unknown }[]) => {
      for (const node of nodes) {
        if (node.type !== "element") continue;
        expect(node.tagName).toBe("span");
        for (const cls of (node.properties as { className?: string[] })?.className ?? [])
          classes.add(cls);
        walk((node.children ?? []) as never);
      }
    };
    walk(tree?.children as never);
    expect([...classes].some((cls) => cls.startsWith("hljs-"))).toBe(true);
  });

  it("declines unknown languages rather than guessing", async () => {
    expect(await highlight("hello", "brainfuck")).toBeNull();
  });
});
