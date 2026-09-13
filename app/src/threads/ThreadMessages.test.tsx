import { render } from "@solidjs/testing-library";
import { describe, expect, it } from "vitest";
import { ThreadMessage } from "./ThreadMessages";
import { emptyHistory } from "./model";
import type { ChatMessage, ProcessOrigin } from "../protocol";

const origin: ProcessOrigin = { kind: "process", process_id: "p1", name: "wakeAfterThirtySeconds", trigger: { kind: "timer", label: "wake", in_secs: 30 }, subscription_key: "trigger-subscription:v2:blake3:secret", outcome: "completed", result: "I'm awake" };
function mount(overrides: Partial<ProcessOrigin> = {}, body = "I'm awake") {
  const message: ChatMessage = { id: 4, thread_id: 1, author: "agent", body, ref: null, ts: "2026-09-13T10:18:00Z", origin: { ...origin, ...overrides } };
  return render(() => <ThreadMessage entry={{ key: "message:4", kind: "message", message }} history={emptyHistory()} threadId={1} />);
}
describe("process delivery notes", () => {
  it("renders a completed timer as one compact note with bare result and no agent bubble", () => {
    const view = mount();
    const note = view.getByRole("article", { name: "Process delivery" });
    expect(note.querySelector('[data-slot="conversation-note"]')).toBeTruthy();
    expect(note).toHaveTextContent("timer · in 30s");
    expect(note).toHaveTextContent("completed");
    expect(view.getByText("I'm awake")).toHaveTextContent(/^I'm awake$/);
    expect(note.querySelector("code")).toHaveTextContent(origin.name);
    expect(note.querySelector("time")).toHaveAttribute("datetime", "2026-09-13T10:18:00Z");
    expect(view.container.querySelector('[data-slot="agent-message"], [data-slot="owner-message"]')).toBeNull();
    expect(note.textContent).not.toContain("blake3");
  });
  it("renders the failure reason visibly with destructive outcome colour", () => {
    const view = mount({ outcome: "failed", error: "Permission denied", result: null }, "Permission denied");
    expect(view.getByText("failed")).toHaveClass("text-destructive");
    expect(view.getAllByText("Permission denied")).toHaveLength(1);
  });
  it("names the triggering Thread and renders structured results as JSON", () => {
    const view = mount({ trigger: { kind: "thread", event: "thread.Report", thread_id: 12, title: "Release checks" }, result: { ok: true } }, '```json\n{\n  "ok": true\n}\n```');
    expect(view.getByText("thread.Report · #12 Release checks")).toBeTruthy();
    expect(view.container.querySelector("pre code")).toHaveTextContent('"ok": true');
    expect(view.container.querySelector('[data-author="agent"]')).toBeNull();
  });
  it("shows cron and cancelled variants and treats scalar strings as literal text", () => {
    const view = mount({ trigger: { kind: "cron", expr: "*/5 * * * *", tz: "UTC" }, outcome: "cancelled", result: "**stopped**" }, "**stopped**");
    expect(view.getByText("cron · */5 * * * * (UTC)")).toBeTruthy();
    expect(view.getByText("cancelled")).toBeTruthy();
    expect(view.getByText("**stopped**")).toBeTruthy();
    expect(view.container.querySelector("strong")).toBeNull();
  });
});
