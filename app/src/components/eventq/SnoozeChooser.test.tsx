import { fireEvent, render, screen } from "@solidjs/testing-library";
import { describe, expect, it, vi } from "vitest";
import { SnoozeChooser } from "./SnoozeChooser";

describe("SnoozeChooser — the durable-snooze preset chooser", () => {
  it("picks a preset, reporting its return instant and label", async () => {
    const onPick = vi.fn();
    render(() => <SnoozeChooser open onOpenChange={() => {}} onPick={onPick} />);
    const evening = await screen.findByText("This evening");
    fireEvent.click(evening);
    expect(onPick).toHaveBeenCalledTimes(1);
    const [until, label] = onPick.mock.calls[0];
    expect(label).toBe("This evening");
    expect(Date.parse(until as string)).toBeGreaterThan(Date.now());
  });

  it("'Pick time…' reveals a datetime-local and reports the chosen instant", async () => {
    const onPick = vi.fn();
    render(() => <SnoozeChooser open onOpenChange={() => {}} onPick={onPick} />);
    fireEvent.click(await screen.findByText("Pick time…"));
    const input = screen.getByLabelText("Snooze until a specific time") as HTMLInputElement;
    fireEvent.input(input, { target: { value: "2026-08-01T09:30" } });
    fireEvent.click(screen.getByRole("button", { name: "Snooze" }));
    expect(onPick).toHaveBeenCalledTimes(1);
    const [until] = onPick.mock.calls[0];
    expect(new Date(until as string).getHours()).toBe(9);
    expect(new Date(until as string).getMinutes()).toBe(30);
  });
});
