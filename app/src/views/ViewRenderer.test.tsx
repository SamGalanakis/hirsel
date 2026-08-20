import { render, within } from "@solidjs/testing-library";
import userEvent from "@testing-library/user-event";
import { createSignal } from "solid-js";
import { describe, expect, it, vi } from "vitest";
import type { ViewSpec } from "../protocol";
import { ViewRenderer } from "./ViewRenderer";

type EmittedEvent = { instanceId: string; action: string; data: unknown };

function renderSpec(spec: ViewSpec, onEvent?: (e: EmittedEvent) => void) {
  return render(() => (
    <ViewRenderer spec={spec} instanceId="view-1" placement="canvas" onEvent={onEvent} />
  ));
}

describe("ViewRenderer — catalog components render", () => {
  it("renders every catalog component type without throwing", () => {
    const spec: ViewSpec = {
      type: "card",
      title: "Review",
      subtitle: "PR #42",
      children: [
        { type: "heading", text: "Summary", level: 2 },
        { type: "text", text: "All checks passed.", tone: "success" },
        { type: "divider" },
        {
          type: "keyValue",
          items: [
            { label: "Branch", value: "feat/x" },
            { label: "Files", value: 12, tone: "muted" },
          ],
        },
        {
          type: "table",
          caption: "Checks",
          columns: [
            { key: "name", label: "Check" },
            { key: "state", label: "State", align: "end" },
          ],
          rows: [{ name: "build", state: "ok" }],
        },
        { type: "list", ordered: true, items: [{ text: "First" }, { text: "Second", tone: "warning" }] },
        {
          type: "checklist",
          items: [
            { label: "Lint", checked: true },
            { label: "Deploy", checked: false, detail: "pending" },
          ],
        },
        {
          type: "row",
          gap: "sm",
          children: [
            { type: "badge", label: "v2", tone: "success" },
            { type: "status", label: "Running", state: "running" },
          ],
        },
        { type: "progress", value: 0.4, label: "Upload" },
        { type: "callout", title: "Heads up", body: "Needs review.", tone: "warning" },
      ],
    };

    const screen = renderSpec(spec);

    expect(screen.getByText("Review")).toBeTruthy();
    expect(screen.getByText("Summary")).toBeTruthy();
    expect(screen.getByText("All checks passed.")).toBeTruthy();
    expect(screen.getByText("Branch")).toBeTruthy();
    expect(screen.getByText("feat/x")).toBeTruthy();
    expect(screen.getByText("build")).toBeTruthy();
    expect(screen.getByText("First")).toBeTruthy();
    expect(screen.getByText("Lint")).toBeTruthy();
    expect(screen.getByText("v2")).toBeTruthy();
    expect(screen.getByText("Running")).toBeTruthy();
    expect(screen.getByText("Upload")).toBeTruthy();
    expect(screen.getByText("Heads up")).toBeTruthy();
    // progress renders a progressbar at the clamped percentage
    expect(screen.getByRole("progressbar").getAttribute("aria-valuenow")).toBe("40");
  });

  it("clamps out-of-range progress values", () => {
    const over = renderSpec({ type: "progress", value: 5 });
    expect(over.getByRole("progressbar").getAttribute("aria-valuenow")).toBe("100");
    const under = renderSpec({ type: "progress", value: -1 });
    expect(under.getByRole("progressbar").getAttribute("aria-valuenow")).toBe("0");
  });

  it("renders `text` as PLAIN text — no HTML injection (safe by vocabulary)", () => {
    const screen = renderSpec({ type: "text", text: "<b>bold</b> & <script>x</script>" });
    // The literal string is present and no <b>/<script> element was created.
    expect(screen.getByText("<b>bold</b> & <script>x</script>")).toBeTruthy();
    expect(screen.container.querySelector("b")).toBeNull();
    expect(screen.container.querySelector("script")).toBeNull();
  });
});

describe("ViewRenderer — graceful degradation", () => {
  it("renders an unknown component type as a placeholder, never throws", () => {
    const screen = renderSpec({ type: "definitely-not-a-real-type" });
    expect(screen.getByText(/Unsupported view component/)).toBeTruthy();
    expect(screen.getByText("definitely-not-a-real-type")).toBeTruthy();
  });

  it("renders an unknown FIELD kind as a placeholder, like an unknown node type", () => {
    const screen = renderSpec({
      type: "form",
      action: "go",
      fields: [
        { type: "field", name: "when", label: "When", kind: "datepicker" },
        { type: "field", name: "who", label: "Who", kind: "text" },
      ],
    });
    expect(screen.getByText(/Unsupported field kind/)).toBeTruthy();
    expect(screen.getByText("datepicker")).toBeTruthy();
    // The rest of the form still renders and submits.
    expect(screen.getByLabelText(/Who/)).toBeTruthy();
    expect(screen.getByRole("button", { name: "Submit" })).toBeTruthy();
  });

  it("seeds an unknown field kind as an empty string", async () => {
    const onEvent = vi.fn();
    const screen = renderSpec(
      {
        type: "form",
        action: "go",
        fields: [{ type: "field", name: "when", label: "When", kind: "datepicker" }],
      },
      onEvent,
    );
    await userEvent.click(screen.getByRole("button", { name: "Submit" }));
    expect(onEvent).toHaveBeenCalledWith({
      instanceId: "view-1",
      action: "go",
      data: { when: "" },
    });
  });

  it("seeds each known kind with its own empty value", async () => {
    const onEvent = vi.fn();
    const screen = renderSpec(
      {
        type: "form",
        action: "go",
        fields: [
          { type: "field", name: "t", label: "T", kind: "text" },
          { type: "field", name: "a", label: "A", kind: "textarea" },
          { type: "field", name: "n", label: "N", kind: "number" },
          { type: "field", name: "g", label: "G", kind: "toggle" },
          { type: "field", name: "s", label: "S", kind: "select", options: [] },
        ],
      },
      onEvent,
    );
    await userEvent.click(screen.getByRole("button", { name: "Submit" }));
    expect(onEvent).toHaveBeenCalledWith({
      instanceId: "view-1",
      action: "go",
      data: { t: "", a: "", n: null, g: false, s: "" },
    });
  });

  it("skips malformed (non-node) children instead of crashing", () => {
    const screen = renderSpec({
      type: "stack",
      children: [null, "oops", 5, { type: "text", text: "survives" }],
    } as unknown as ViewSpec);
    expect(screen.getByText("survives")).toBeTruthy();
  });

  it("tolerates a known component with missing/invalid props", () => {
    // table with no columns/rows, badge with no label — must not throw.
    const screen = renderSpec({
      type: "stack",
      children: [{ type: "table" }, { type: "badge" }, { type: "keyValue" }],
    });
    expect(screen.container).toBeTruthy();
  });
});

describe("ViewRenderer — event round-trip", () => {
  it("action emits view_event with the declared action + data", async () => {
    const onEvent = vi.fn();
    const screen = renderSpec(
      { type: "action", label: "Approve", action: "approve", data: { pr: 42 } },
      onEvent,
    );
    await userEvent.click(screen.getByRole("button", { name: "Approve" }));
    expect(onEvent).toHaveBeenCalledTimes(1);
    expect(onEvent).toHaveBeenCalledWith({
      instanceId: "view-1",
      action: "approve",
      data: { pr: 42 },
    });
  });

  it("action with no data emits null data", async () => {
    const onEvent = vi.fn();
    const screen = renderSpec({ type: "action", label: "Go", action: "go" }, onEvent);
    await userEvent.click(screen.getByRole("button", { name: "Go" }));
    expect(onEvent).toHaveBeenCalledWith({ instanceId: "view-1", action: "go", data: null });
  });

  it("action shows a pending/disabled state after submit", async () => {
    const onEvent = vi.fn();
    const screen = renderSpec({ type: "action", label: "Send", action: "send" }, onEvent);
    const btn = screen.getByRole("button", { name: "Send" }) as HTMLButtonElement;
    expect(btn.disabled).toBe(false);
    await userEvent.click(btn);
    expect(btn.disabled).toBe(true);
  });

  it("optionSet emits the declared action with { value } for the chosen option", async () => {
    const onEvent = vi.fn();
    const screen = renderSpec(
      {
        type: "optionSet",
        action: "decide",
        label: "How to proceed?",
        choices: [
          { label: "Ship it", value: "ship" },
          { label: "Hold", value: "hold", description: "wait for review" },
        ],
      },
      onEvent,
    );
    await userEvent.click(screen.getByRole("button", { name: /Hold/ }));
    expect(onEvent).toHaveBeenCalledWith({
      instanceId: "view-1",
      action: "decide",
      data: { value: "hold" },
    });
  });

  it("form emits an object keyed by field name", async () => {
    const onEvent = vi.fn();
    const screen = renderSpec(
      {
        type: "form",
        action: "submit_feedback",
        submitLabel: "Save",
        fields: [
          { type: "field", name: "title", label: "Title", kind: "text", value: "seed" },
          { type: "field", name: "notes", label: "Notes", kind: "textarea" },
          { type: "field", name: "count", label: "Count", kind: "number" },
          { type: "field", name: "urgent", label: "Urgent", kind: "toggle" },
        ],
      },
      onEvent,
    );

    const title = screen.getByLabelText(/Title/) as HTMLInputElement;
    await userEvent.clear(title);
    await userEvent.type(title, "Hello");
    await userEvent.type(screen.getByLabelText(/Notes/) as HTMLTextAreaElement, "body text");
    await userEvent.type(screen.getByLabelText(/Count/) as HTMLInputElement, "7");
    await userEvent.click(screen.getByRole("checkbox"));

    await userEvent.click(screen.getByRole("button", { name: "Save" }));

    expect(onEvent).toHaveBeenCalledTimes(1);
    expect(onEvent).toHaveBeenCalledWith({
      instanceId: "view-1",
      action: "submit_feedback",
      data: { title: "Hello", notes: "body text", count: 7, urgent: true },
    });
  });

  it("form seeds values from declared field defaults", async () => {
    const onEvent = vi.fn();
    const screen = renderSpec(
      {
        type: "form",
        action: "go",
        fields: [{ type: "field", name: "who", label: "Who", kind: "text", value: "prefilled" }],
      },
      onEvent,
    );
    await userEvent.click(screen.getByRole("button", { name: "Submit" }));
    expect(onEvent).toHaveBeenCalledWith({
      instanceId: "view-1",
      action: "go",
      data: { who: "prefilled" },
    });
  });

  it("select field emits the chosen option value", async () => {
    const onEvent = vi.fn();
    const screen = renderSpec(
      {
        type: "form",
        action: "pick",
        fields: [
          {
            type: "field",
            name: "color",
            label: "Color",
            kind: "select",
            value: "red",
            options: [
              { label: "Red", value: "red" },
              { label: "Blue", value: "blue" },
            ],
          },
        ],
      },
      onEvent,
    );
    await userEvent.selectOptions(screen.getByLabelText(/Color/) as HTMLSelectElement, "blue");
    await userEvent.click(screen.getByRole("button", { name: "Submit" }));
    expect(onEvent).toHaveBeenCalledWith({
      instanceId: "view-1",
      action: "pick",
      data: { color: "blue" },
    });
  });

  it("does not emit until the control is actually activated", () => {
    const onEvent = vi.fn();
    renderSpec({ type: "action", label: "Idle", action: "noop" }, onEvent);
    expect(onEvent).not.toHaveBeenCalled();
  });
});

describe("ViewRenderer — update in place", () => {
  it("re-renders when the same instance's spec changes", async () => {
    const [spec, setSpec] = createSignal<ViewSpec>({
      type: "progress",
      value: 0.2,
      label: "Step 1",
    });
    const screen = render(() => (
      <ViewRenderer spec={spec()} instanceId="view-1" placement="canvas" />
    ));
    expect(screen.getByRole("progressbar").getAttribute("aria-valuenow")).toBe("20");
    expect(screen.getByText("Step 1")).toBeTruthy();

    setSpec({ type: "progress", value: 0.75, label: "Step 3" });
    expect(screen.getByRole("progressbar").getAttribute("aria-valuenow")).toBe("75");
    expect(screen.getByText("Step 3")).toBeTruthy();
  });

  it("re-renders across a root component type change on the same instance", () => {
    const [spec, setSpec] = createSignal<ViewSpec>({ type: "text", text: "before" });
    const screen = render(() => (
      <ViewRenderer spec={spec()} instanceId="view-1" placement="canvas" />
    ));
    expect(screen.getByText("before")).toBeTruthy();
    setSpec({ type: "badge", label: "after", tone: "success" });
    expect(screen.getByText("after")).toBeTruthy();
    expect(screen.queryByText("before")).toBeNull();
  });
});

describe("ViewRenderer — structure", () => {
  it("tags the view root with its placement", () => {
    const screen = renderSpec({ type: "text", text: "hi" });
    const root = screen.container.querySelector('[data-slot="view"]');
    expect(root?.getAttribute("data-placement")).toBe("canvas");
    within(root as HTMLElement).getByText("hi");
  });
});
