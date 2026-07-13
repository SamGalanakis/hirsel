import { fireEvent, render, within } from "@solidjs/testing-library";
import { describe, expect, it, vi } from "vitest";
import type { ViewSpec } from "../protocol";
import { EventCardRenderer } from "./EventCardRenderer";

type Emitted = { action: string; data: unknown };

function renderCard(ui: ViewSpec | ViewSpec[], onAction?: (a: string, d: unknown) => void) {
  return render(() => <EventCardRenderer ui={ui} onAction={onAction} />);
}

describe("EventCardRenderer — constrained vocabulary", () => {
  it("renders every node type without throwing, and a card root unwraps", () => {
    const ui: ViewSpec = {
      type: "card",
      children: [
        { type: "eyebrow", tone: "accent", boundary: true, text: "Taste boundary" },
        { type: "heading", text: "Which way to wire `reopen`?" },
        { type: "text", tone: "muted", text: "context here" },
        { type: "divider" },
        { type: "keyValue", items: [{ label: "unblocks", value: "1 agent" }] },
        { type: "badge", label: "judgment", tone: "success" },
        { type: "status", state: "success", label: "CI green" },
        {
          type: "optionList",
          action: "choose",
          options: [
            { key: "A", recommended: true, label: "Option A", detail: "tradeoff" },
            { key: "B", label: "Option B" },
          ],
        },
        {
          type: "viewSlot",
          variant: "diff",
          title: "the diff",
          lines: [
            { op: "add", text: "added line" },
            { op: "del", text: "removed line" },
            { op: "ctx", text: "context line" },
          ],
        },
        {
          type: "viewSlot",
          variant: "table",
          title: "digest",
          rows: [{ label: "job", value: "done", state: "success" }],
        },
      ],
    };
    const screen = renderCard(ui);
    expect(screen.getByText(/Taste boundary/)).toBeTruthy();
    expect(screen.getByText("Option A")).toBeTruthy();
    // `backtick` → a mono span (Monospace-Earns-It), and the prose around it stays text.
    expect(screen.getByText("reopen").tagName).toBe("SPAN");
    expect(screen.getByText("reopen").className).toContain("font-mono");
  });

  it("degrades an unknown node to a fallback chip and never throws or loses siblings", () => {
    const ui: ViewSpec[] = [
      { type: "totally-unknown-node" },
      { type: "heading", text: "still here" },
    ];
    const screen = renderCard(ui);
    expect(screen.getByText(/unsupported node/)).toBeTruthy();
    expect(screen.getByText("totally-unknown-node")).toBeTruthy();
    // The sibling after the unknown node still renders.
    expect(screen.getByText("still here")).toBeTruthy();
  });

  it("emits `choose` with {choice, label} when an option is tapped", () => {
    const onAction = vi.fn<(a: string, d: unknown) => void>();
    const screen = renderCard(
      [
        {
          type: "optionList",
          action: "choose",
          options: [{ key: "A", recommended: true, label: "New `reopen_ping` op" }],
        },
      ],
      onAction,
    );
    fireEvent.click(screen.getByRole("button", { name: /New/ }));
    expect(onAction).toHaveBeenCalledTimes(1);
    const [action, data] = onAction.mock.calls[0] as [string, Emitted["data"]];
    expect(action).toBe("choose");
    // The label is stripped of backtick markers in the payload.
    expect(data).toEqual({ choice: "A", label: "New reopen_ping op" });
  });

  it("collects the card's field values and posts them on submit", () => {
    const onAction = vi.fn<(a: string, d: unknown) => void>();
    const screen = renderCard(
      [
        {
          type: "inset",
          children: [
            { type: "field", name: "note", label: "Standing rule", placeholder: "…" },
            { type: "submit", action: "choose_with_rule", label: "Ship + record rule" },
          ],
        },
      ],
      onAction,
    );
    const input = screen.getByPlaceholderText("…") as HTMLInputElement;
    fireEvent.input(input, { target: { value: "always dense rows" } });
    fireEvent.click(screen.getByRole("button", { name: /Ship/ }));
    expect(onAction).toHaveBeenCalledWith("choose_with_rule", { note: "always dense rows" });
  });

  it("disables interactive controls when the card is already decided", () => {
    const onAction = vi.fn<(a: string, d: unknown) => void>();
    const screen = render(() => (
      <EventCardRenderer
        ui={[{ type: "optionList", action: "choose", options: [{ key: "A", label: "Alpha" }] }]}
        onAction={onAction}
        disabled
      />
    ));
    const btn = within(screen.container).getByRole("button", { name: /Alpha/ }) as HTMLButtonElement;
    expect(btn.disabled).toBe(true);
    fireEvent.click(btn);
    expect(onAction).not.toHaveBeenCalled();
  });
});
