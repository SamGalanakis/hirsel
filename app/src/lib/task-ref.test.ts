import { describe, expect, it } from "vitest";
import { EventKind } from "../protocol";
import type { EventItem } from "../protocol";
import {
  detectRefQuery,
  filterTaskCandidates,
  formatTaskRef,
  insertTaskRef,
  parseTaskRef,
  resolveMentionIds,
  splitTaskRefs,
} from "./task-ref";

function task(overrides: Partial<EventItem> = {}): EventItem {
  return {
    id: 1,
    kind: EventKind.Judgment,
    source: { kind: "agent", ref: "host" },
    name: "deploy-4821",
    description: "Ship build 4821?",
    ui: [],
    requires_response: true,
    quick_replies: [],
    status: "open",
    read: false,
    anchor: 10,
    ts: "2026-08-18T10:00:00Z",
    ...overrides,
  };
}

const field = [
  task({ id: 1, name: "deploy-4821" }),
  task({ id: 2, name: "auth-pr" }),
  task({ id: 3, name: "nightly-backup" }),
  task({ id: 12, name: "deploy-notes" }),
];

describe("formatTaskRef / parseTaskRef", () => {
  it("is one spelling, and round-trips", () => {
    expect(formatTaskRef(12)).toBe("#12");
    expect(parseTaskRef("#12")).toBe(12);
    expect(parseTaskRef(" 12 ")).toBe(12);
  });

  it("refuses anything that is not a task id", () => {
    expect(parseTaskRef("#")).toBeNull();
    expect(parseTaskRef("#0")).toBeNull();
    expect(parseTaskRef("#1a")).toBeNull();
    expect(parseTaskRef("#-3")).toBeNull();
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

describe("filterTaskCandidates", () => {
  it("lists the whole field, newest-first, for an empty query", () => {
    expect(filterTaskCandidates(field, "").map((t) => t.id)).toEqual([12, 3, 2, 1]);
  });

  it("puts an exact id first, then id prefixes, then names", () => {
    expect(filterTaskCandidates(field, "1").map((t) => t.id)).toEqual([1, 12]);
    expect(filterTaskCandidates(field, "12").map((t) => t.id)).toEqual([12]);
  });

  it("matches names by prefix ahead of substring", () => {
    expect(filterTaskCandidates(field, "deploy").map((t) => t.id)).toEqual([12, 1]);
    expect(filterTaskCandidates(field, "backup").map((t) => t.id)).toEqual([3]);
    expect(filterTaskCandidates(field, "zzz")).toEqual([]);
  });

  it("honours the cap", () => {
    expect(filterTaskCandidates(field, "", 2).map((t) => t.id)).toEqual([12, 3]);
  });
});

describe("insertTaskRef", () => {
  it("replaces the in-progress query with the ref and a trailing space", () => {
    const text = "same as #depl";
    const query = detectRefQuery(text, text.length)!;
    const next = insertTaskRef(text, query, text.length, 1);
    expect(next.text).toBe("same as #1 ");
    expect(next.caret).toBe(next.text.length);
  });

  it("never doubles an existing separator, and still lands past it", () => {
    const text = "same as #depl now";
    const query = detectRefQuery(text, 13)!;
    const next = insertTaskRef(text, query, 13, 12);
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
    const picked = filterTaskCandidates(field, query.query)[0];
    const next = insertTaskRef(typed, query, typed.length, picked.id);
    expect(resolveMentionIds(next.text, field)).toEqual([picked.id]);
  });

  it("ignores refs that name nothing in the field", () => {
    expect(resolveMentionIds("what about #99?", field)).toEqual([]);
  });

  it("is not fooled by mid-word hashes or hex colours", () => {
    expect(resolveMentionIds("ab#12 and #1234ab", field)).toEqual([]);
  });
});

describe("splitTaskRefs", () => {
  const known = (id: number) => field.some((t) => t.id === id);

  it("lifts a live ref out of its prose", () => {
    expect(splitTaskRefs("done with #2 now", known)).toEqual([
      { text: "done with ", taskId: null },
      { text: "#2", taskId: 2 },
      { text: " now", taskId: null },
    ]);
  });

  it("leaves an unknown or archived ref as the literal characters typed", () => {
    expect(splitTaskRefs("gone: #99", known)).toEqual([{ text: "gone: #99", taskId: null }]);
  });

  it("keeps text with no refs in one span", () => {
    expect(splitTaskRefs("nothing to cite", known)).toEqual([
      { text: "nothing to cite", taskId: null },
    ]);
  });
});
