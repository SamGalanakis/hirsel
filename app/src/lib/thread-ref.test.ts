import { describe, expect, it } from "vitest";
import type { RefTarget } from "./thread-ref";
import {
  detectRefQuery,
  filterThreadCandidates,
  formatThreadRef,
  insertThreadRef,
  parseThreadRef,
  resolveMentionIds,
  splitThreadRefs,
} from "./thread-ref";

function thread(overrides: Partial<RefTarget> = {}): RefTarget { return { id: 1, kind: "space", title: "deploy-4821", ...overrides }; }

const field = [
  thread({ id: 1, title: "deploy-4821" }),
  thread({ id: 2, title: "auth-pr" }),
  thread({ id: 3, title: "nightly-backup" }),
  thread({ id: 12, title: "deploy-notes" }),
];

describe("formatThreadRef / parseThreadRef", () => {
  it("is one spelling, and round-trips", () => {
    expect(formatThreadRef(12)).toBe("#12");
    expect(parseThreadRef("#12")).toBe(12);
    expect(parseThreadRef(" 12 ")).toBe(12);
  });

  it("refuses anything that is not a thread id", () => {
    expect(parseThreadRef("#")).toBeNull();
    expect(parseThreadRef("#0")).toBe(0);
    expect(parseThreadRef("#1a")).toBeNull();
    expect(parseThreadRef("#-3")).toBeNull();
  });
});

describe("detectRefQuery", () => {
  it("opens on a lone # at the caret (empty query)", () => {
    expect(detectRefQuery("#", 1)).toEqual({ start: 0, query: "" });
    expect(detectRefQuery("same as #", 9)).toEqual({ start: 8, query: "" });
  });

  it("captures the partial query up to the caret, digits or name", () => {
    expect(detectRefQuery("see #48", 7)).toEqual({ start: 4, query: "48" });
    expect(detectRefQuery("see #depl", 9)).toEqual({ start: 4, query: "depl" });
    expect(detectRefQuery("#deploy-4821", 7)).toEqual({ start: 0, query: "deploy" });
  });

  it("stays shut mid-word and once the token is behind the caret", () => {
    expect(detectRefQuery("colour ab#4", 11)).toBeNull();
    expect(detectRefQuery("#12 done", 8)).toBeNull();
    expect(detectRefQuery("nothing here", 7)).toBeNull();
  });
});

describe("filterThreadCandidates", () => {
  it("lists the whole field, newest-first, for an empty query", () => {
    expect(filterThreadCandidates(field, "").map((t) => t.id)).toEqual([12, 3, 2, 1]);
  });

  it("puts an exact id first, then id prefixes, then names", () => {
    expect(filterThreadCandidates(field, "1").map((t) => t.id)).toEqual([1, 12]);
    expect(filterThreadCandidates(field, "12").map((t) => t.id)).toEqual([12]);
  });

  it("matches names by prefix ahead of substring", () => {
    expect(filterThreadCandidates(field, "deploy").map((t) => t.id)).toEqual([12, 1]);
    expect(filterThreadCandidates(field, "backup").map((t) => t.id)).toEqual([3]);
    expect(filterThreadCandidates(field, "zzz")).toEqual([]);
  });

  it("honours the cap", () => {
    expect(filterThreadCandidates(field, "", 2).map((t) => t.id)).toEqual([12, 3]);
  });
});

describe("insertThreadRef", () => {
  it("replaces the in-progress query with the ref and a trailing space", () => {
    const text = "same as #depl";
    const query = detectRefQuery(text, text.length)!;
    const next = insertThreadRef(text, query, text.length, 1);
    expect(next.text).toBe("same as #1 ");
    expect(next.caret).toBe(next.text.length);
  });

  it("never doubles an existing separator, and still lands past it", () => {
    const text = "same as #depl now";
    const query = detectRefQuery(text, 13)!;
    const next = insertThreadRef(text, query, 13, 12);
    expect(next.text).toBe("same as #12 now");
    expect(next.text.slice(0, next.caret)).toBe("same as #12 ");
  });
});

describe("resolveMentionIds", () => {
  it("re-derives mentions from the composed body, deduped and in order", () => {
    expect(resolveMentionIds("look at #12 and #2, then #12 again", field)).toEqual([12, 2]);
  });

  it("round-trips a picked ref", () => {
    const text = "check ";
    const typed = `${text}#`;
    const query = detectRefQuery(typed, typed.length)!;
    const picked = filterThreadCandidates(field, query.query)[0];
    const next = insertThreadRef(typed, query, typed.length, picked.id);
    expect(resolveMentionIds(next.text, field)).toEqual([picked.id]);
  });

  it("ignores refs that name nothing in the field", () => {
    expect(resolveMentionIds("what about #99?", field)).toEqual([]);
  });

  it("is not fooled by mid-word hashes or hex colours", () => {
    expect(resolveMentionIds("ab#12 and #1234ab", field)).toEqual([]);
  });
});

describe("splitThreadRefs", () => {
  const known = (id: number) => field.some((t) => t.id === id);

  it("lifts a live ref out of its prose", () => {
    expect(splitThreadRefs("done with #2 now", known)).toEqual([
      { text: "done with ", threadId: null },
      { text: "#2", threadId: 2 },
      { text: " now", threadId: null },
    ]);
  });

  it("leaves an unknown or archived ref as the literal characters typed", () => {
    expect(splitThreadRefs("gone: #99", known)).toEqual([{ text: "gone: #99", threadId: null }]);
  });

  it("keeps text with no refs in one span", () => {
    expect(splitThreadRefs("nothing to cite", known)).toEqual([
      { text: "nothing to cite", threadId: null },
    ]);
  });
});
