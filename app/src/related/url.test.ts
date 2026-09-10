import { describe, expect, it } from "vitest";
import { isBareLinkLabel, webLink } from "./url";
describe("local link recognition", () => {
  it("recognizes exact resource identities without rewriting the destination", () => {
    const input = "https://github.com/owner/repo/pull/123/files?diff=split#discussion";
    expect(webLink(input)).toMatchObject({ url: input, label: "owner/repo · PR #123", kind: "pull-request", site: "GitHub" });
    expect(webLink("https://github.com/owner/repo/issues/3")?.kind).toBe("issue");
    expect(webLink("https://github.com/owner/repo")?.kind).toBe("repository");
  });
  it("recognizes Linear issues without rewriting the destination", () => {
    const input = "https://linear.app/acme/issue/ENG-123/fix-the-parser?pane=activity#comment-42";
    expect(webLink(input)).toMatchObject({ url: input, label: "ENG-123", kind: "issue", site: "Linear" });
    expect(webLink("https://linear.app/acme/issue/eng-7")?.label).toBe("ENG-7");
  });
  it("rejects unsafe or nonabsolute destinations and keeps lookalikes generic", () => {
    for (const value of ["javascript:alert(1)", "data:text/html,x", "mailto:a@b.com", "/relative", "//github.com/a/b", " https://github.com/a/b", "https://u:p@github.com/a/b", "https://u:p@linear.app/acme/issue/ENG-1", "https://example.com/a\nb", "https://example.com/" + "a".repeat(4096)]) expect(webLink(value), value).toBeNull();
    for (const value of ["https://github.com.evil.test/a/b/pull/1", "https://evil.test/github.com/a/b/pull/1", "https://github.com:1234/a/b/pull/1", "https://github.com/a/b/pull/not-a-number"]) expect(webLink(value)?.kind, value).toBe("web");
    for (const value of ["https://linear.app.evil.test/acme/issue/ENG-1", "https://evil.test/linear.app/acme/issue/ENG-1", "https://linear.app:1234/acme/issue/ENG-1", "https://linear.app/acme/issue/not-a-ticket", "https://linear.app/acme/issues/ENG-1", "https://linear.app/acme/issue/ENG-1/slug/extra"]) expect(webLink(value)?.kind, value).toBe("web");
  });
  it("normalizes URL syntax while preserving query and fragment identity", () => {
    expect(webLink("HTTPS://Example.COM:443/a?x=1#part")?.url).toBe("https://example.com/a?x=1#part");
    expect(webLink("https://example.com/a?x=1")?.url).not.toBe(webLink("https://example.com/a?x=2")?.url);
  });
  it("only substitutes labels that are themselves the URL", () => {
    expect(isBareLinkLabel("Review the change", "https://github.com/a/b/pull/1")).toBe(false);
    expect(isBareLinkLabel("https://example.com", "https://example.com")).toBe(true);
    expect(isBareLinkLabel("www.example.com", "http://www.example.com")).toBe(true);
  });
});
