import { describe, expect, it } from "vitest";
import type { Ping } from "../../protocol";
import {
  detectMentionQuery,
  filterMentionCandidates,
  insertMention,
  resolveMentionIds,
} from "./mentions";

function ping(overrides: Partial<Ping> = {}): Ping {
  return {
    id: 1,
    name: "release-choice",
    description: "Choose the release channel",
    content: "Which channel?",
    anchor: 5,
    requires_response: false,
    quick_replies: [],
    status: "open",
    ts: "2026-07-10T00:00:00Z",
    ...overrides,
  };
}

describe("detectMentionQuery", () => {
  it("opens on a lone @ at the caret (empty query)", () => {
    expect(detectMentionQuery("@", 1)).toEqual({ start: 0, query: "" });
    expect(detectMentionQuery("hey @", 5)).toEqual({ start: 4, query: "" });
  });

  it("captures the partial handle up to the caret", () => {
    expect(detectMentionQuery("ping @rel", 9)).toEqual({ start: 5, query: "rel" });
    expect(detectMentionQuery("@release-choice", 8)).toEqual({ start: 0, query: "release" });
  });

  it("does not trigger mid-word (email-like a@b) or after a space break", () => {
    expect(detectMentionQuery("mail a@b", 8)).toBeNull();
    expect(detectMentionQuery("@rel foo", 8)).toBeNull();
  });
});

describe("filterMentionCandidates", () => {
  const pings = [
    ping({ id: 1, name: "release-choice" }),
    ping({ id: 2, name: "deploy-window" }),
    ping({ id: 3, name: "release-notes" }),
    ping({ id: 4, name: "old-thing", status: "done" }),
  ];

  it("lists only open pings, all of them, for an empty query", () => {
    const out = filterMentionCandidates(pings, "");
    expect(out.map((p) => p.id)).toEqual([3, 2, 1]); // open only, newest-first
  });

  it("substring-matches by name and sorts startsWith ahead", () => {
    const out = filterMentionCandidates(pings, "rel");
    expect(out.map((p) => p.name)).toEqual(["release-notes", "release-choice"]);
  });

  it("never surfaces a done ping", () => {
    expect(filterMentionCandidates(pings, "old")).toEqual([]);
  });
});

describe("insertMention", () => {
  it("replaces the in-progress @query with @name and a trailing space", () => {
    const text = "look at @rel now";
    const q = detectMentionQuery(text, 12)!; // caret after "@rel"
    const { text: next, caret } = insertMention(text, q, 12, "release-choice");
    expect(next).toBe("look at @release-choice  now");
    expect(next.slice(0, caret)).toBe("look at @release-choice ");
  });
});

describe("resolveMentionIds", () => {
  const pings = [
    ping({ id: 7, name: "release-choice", status: "open" }),
    ping({ id: 8, name: "deploy-window", status: "open" }),
    ping({ id: 9, name: "done-thing", status: "done" }),
  ];

  it("resolves @handle tokens to open ping ids, deduped and order-preserving", () => {
    expect(resolveMentionIds("ping @release-choice and @deploy-window", pings)).toEqual([7, 8]);
    expect(resolveMentionIds("@release-choice @release-choice", pings)).toEqual([7]);
  });

  it("ignores unknown handles and done pings and mid-word @", () => {
    expect(resolveMentionIds("@unknown @done-thing a@b", pings)).toEqual([]);
  });
});
