import { fireEvent, render } from "@solidjs/testing-library";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { EventKind } from "../../protocol";
import type { EventItem } from "../../protocol";

// A cited Task inside conversation prose: it renders as an inline tag, opens
// that Task when activated, and degrades to the literal characters typed when
// it names nothing the field still holds.

beforeEach(() => {
  vi.resetModules();
});

function task(id: number, name: string): EventItem {
  return {
    id,
    kind: EventKind.Judgment,
    source: { kind: "agent", ref: "host" },
    name,
    description: name,
    ui: [],
    requires_response: true,
    quick_replies: [],
    status: "open",
    read: false,
    anchor: id * 10,
    ts: "2026-08-18T10:00:00Z",
  };
}

async function seedField(tasks: EventItem[]) {
  const store = await import("../../store/store");
  for (const event of tasks) {
    store.dispatch({ type: "event_upsert", payload: { type: "event_upsert", event } });
  }
  return store;
}

async function renderBody(body: string) {
  const { Markdown } = await import("../Markdown");
  return render(() => <Markdown>{body}</Markdown>);
}

describe("a cited Task in conversation", () => {
  it("renders as an inline tag naming the Task", async () => {
    await seedField([task(2, "auth-pr")]);
    const { container } = await renderBody("picked this up in #2 yesterday");
    const tag = container.querySelector<HTMLElement>('[data-slot="task-ref-tag"]');
    expect(tag).not.toBeNull();
    expect(tag?.dataset.taskRef).toBe("2");
    expect(tag?.textContent).toContain("#2");
    expect(tag?.getAttribute("aria-label")).toBe("auth pr, task #2");
    expect(container.textContent).toBe("picked this up in #2 yesterday");
  });

  it("focuses that Task when activated", async () => {
    const store = await seedField([task(2, "auth-pr")]);
    const { container } = await renderBody("see #2");
    expect(store.state.focusedTaskId).toBeNull();
    fireEvent.click(container.querySelector('[data-slot="task-ref-tag"]') as HTMLElement);
    expect(store.state.focusedTaskId).toBe(2);
  });

  it("marks the Task you are already in rather than offering to re-enter it", async () => {
    const store = await seedField([task(2, "auth-pr")]);
    store.focusTask(2);
    const { container } = await renderBody("see #2");
    expect(
      container.querySelector('[data-slot="task-ref-tag"]')?.getAttribute("aria-current"),
    ).toBe("page");
  });

  it("degrades an unknown ref to plain text", async () => {
    await seedField([task(2, "auth-pr")]);
    const { container } = await renderBody("what about #99?");
    expect(container.querySelector('[data-slot="task-ref-tag"]')).toBeNull();
    expect(container.textContent).toBe("what about #99?");
  });

  it("leaves a ref inside code alone", async () => {
    await seedField([task(2, "auth-pr")]);
    const { container } = await renderBody("run `git show #2` first");
    expect(container.querySelector('[data-slot="task-ref-tag"]')).toBeNull();
    expect(container.querySelector("code")?.textContent).toBe("git show #2");
  });
});
