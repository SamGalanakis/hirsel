import { render } from "@solidjs/testing-library";
import userEvent from "@testing-library/user-event";

import { describe, expect, it, vi } from "vitest";
import type { ViewSpec } from "../protocol";
import { ViewRenderer } from "./ViewRenderer";

type EmittedEvent = { instanceId: string; action: string; data: unknown };

function renderSpec(spec: ViewSpec, onEvent?: (e: EmittedEvent) => void) {
  return render(() => (
    <ViewRenderer spec={spec} instanceId="view-1" onEvent={onEvent} />
  ));
}

describe("ViewRenderer — catalog components render", () => {
  it("renders `text` as PLAIN text — no HTML injection (safe by vocabulary)", () => {
    const screen = renderSpec({ type: "text", text: "<b>bold</b> & <script>x</script>" });
    // The literal string is present and no <b>/<script> element was created.
    expect(screen.getByText("<b>bold</b> & <script>x</script>")).toBeTruthy();
    expect(screen.container.querySelector("b")).toBeNull();
    expect(screen.container.querySelector("script")).toBeNull();
  });
});

describe("ViewRenderer — graceful degradation", () => {
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
});
