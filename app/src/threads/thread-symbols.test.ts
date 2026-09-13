import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";
import { THREAD_SYMBOL_ART } from "./thread-symbol-art";
import { matchesThreadSymbol, threadMonogram, THREAD_SYMBOLS, THREAD_TINTS } from "./thread-symbols";

// vitest runs with the app directory as its root.
const rust = readFileSync(resolve(process.cwd(), "../crates/hirsel-proto/src/thread_icon.rs"), "utf8");
const between = (start: string, end: string) => rust.slice(rust.indexOf(start) + start.length, rust.indexOf(end, rust.indexOf(start)));

describe("the Thread symbol vocabulary", () => {
  it("matches the Host's list exactly, in order", () => {
    const names = [...between("pub const THREAD_SYMBOLS: [&str; 45] = [", "];").matchAll(/"([a-z-]+)"/g)].map(match => match[1]);
    expect(names).toEqual([...THREAD_SYMBOLS]);
  });
  it("matches the Host's tint palette exactly, in order", () => {
    const tints = [...between("pub const fn as_str(self) -> &'static str {", "\n    }").matchAll(/=> "([a-z]+)"/g)].map(match => match[1]);
    expect(tints).toEqual([...THREAD_TINTS]);
  });
  it("has drawable artwork for every name", () => {
    for (const name of THREAD_SYMBOLS) {
      expect(THREAD_SYMBOL_ART[name].length, name).toBeGreaterThan(0);
      for (const path of THREAD_SYMBOL_ART[name]) expect(path, name).toMatch(/^[Mm]/);
    }
    expect(Object.keys(THREAD_SYMBOL_ART)).toEqual([...THREAD_SYMBOLS]);
  });
  it("searches over whole words and hyphenated parts", () => {
    expect(matchesThreadSymbol("git-branch", "git b")).toBe(true);
    expect(matchesThreadSymbol("git-branch", "branch")).toBe(true);
    expect(matchesThreadSymbol("git-branch", "")).toBe(true);
    expect(matchesThreadSymbol("hammer", "git")).toBe(false);
  });
});

describe("the monogram default", () => {
  it("takes one letter from a single word and two from the first two words", () => {
    expect(threadMonogram("Garden")).toBe("G");
    expect(threadMonogram("Orchard Beds")).toBe("OB");
    expect(threadMonogram("  rebuild the android apk ")).toBe("RT");
    expect(threadMonogram("étude finale")).toBe("ÉF");
  });
  it("falls back to # for an empty title", () => {
    expect(threadMonogram("")).toBe("#");
    expect(threadMonogram("   ")).toBe("#");
  });
});
