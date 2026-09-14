import { fireEvent, render } from "@solidjs/testing-library";
import { describe, expect, it, vi } from "vitest";
import type { ViewSpec } from "../protocol";
import { ThreadInstrument } from "./ThreadInstrument";

type Emitted = { action: string; data: unknown };

function renderCard(ui: ViewSpec | ViewSpec[], onAction?: (a: string, d: unknown, settles: boolean) => void) {
  return render(() => <ThreadInstrument ui={ui} onAction={onAction} />);
}

describe("ThreadInstrument — constrained vocabulary", () => {
  it("emits `choose` with {choice, label} when an option is tapped", () => {
    const onAction = vi.fn<(a: string, d: unknown, settles: boolean) => void>();
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
    const [action, data, settles] = onAction.mock.calls[0] as [string, Emitted["data"], boolean];
    expect(action).toBe("choose");
    // The label is stripped of backtick markers in the payload.
    expect(data).toEqual({ choice: "A", label: "New reopen_ping op" });
    expect(settles).toBe(true);
  });

  it("marks an adaptive option list as non-settling when declared", () => {
    const onAction = vi.fn<(a: string, d: unknown, settles: boolean) => void>();
    const screen = renderCard({
      type: "optionList",
      action: "advance",
      settles: false,
      options: [{ key: "A", label: "Start canary" }],
    }, onAction);
    fireEvent.click(screen.getByRole("button", { name: /Start canary/ }));
    expect(onAction).toHaveBeenCalledWith(
      "advance",
      { choice: "A", label: "Start canary" },
      false,
    );
  });

  it("collects the card's field values and posts them on submit", () => {
    const onAction = vi.fn<(a: string, d: unknown, settles: boolean) => void>();
    const screen = renderCard(
      [
        {
          type: "inset",
          children: [
            { type: "field", name: "note", label: "Standing rule", placeholder: "…" },
            { type: "submit", action: "choose_with_rule", label: "Ship + record rule", kbd: "⌘↵" },
          ],
        },
      ],
      onAction,
    );
    const input = screen.getByPlaceholderText("…") as HTMLInputElement;
    fireEvent.input(input, { target: { value: "always dense rows" } });
    fireEvent.click(screen.getByRole("button", { name: /Ship/ }));
    expect(onAction).toHaveBeenCalledWith("choose_with_rule", { note: "always dense rows" }, true);
    expect(screen.queryByText("⌘↵")).toBeNull();
  });
});
