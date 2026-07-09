import { fireEvent, render } from "@solidjs/testing-library";
import { describe, expect, it } from "vitest";
import { CommittedToolCalls } from "./ToolCalls";

describe("CommittedToolCalls (footer chip expansion)", () => {
  it("renders nothing when there are no tool calls", () => {
    const { container } = render(() => <CommittedToolCalls toolCalls={[]} />);
    expect(container.querySelector('[data-slot="committed-tool-calls"]')).toBeNull();
  });

  it("shows a collapsed 'N tools' chip and expands to the per-tool ok/fail list", () => {
    const { getByRole, queryByText, getByText } = render(() => (
      <CommittedToolCalls
        toolCalls={[
          { name: "read_file", ok: true },
          { name: "shell", ok: false },
        ]}
      />
    ));

    const chip = getByRole("button");
    // Collapsed by default: chip labels the count, list is not rendered.
    expect(chip.getAttribute("aria-expanded")).toBe("false");
    expect(chip.textContent).toContain("2 tools");
    expect(queryByText("read_file")).toBeNull();

    // Expand → both tool names appear, with an ok and a failed marker.
    fireEvent.click(chip);
    expect(chip.getAttribute("aria-expanded")).toBe("true");
    expect(getByText("read_file")).toBeTruthy();
    expect(getByText("shell")).toBeTruthy();
    expect(document.querySelector('[aria-label="ok"]')).toBeTruthy();
    expect(document.querySelector('[aria-label="failed"]')).toBeTruthy();

    // Collapse again.
    fireEvent.click(chip);
    expect(chip.getAttribute("aria-expanded")).toBe("false");
    expect(queryByText("read_file")).toBeNull();
  });

  it("uses the singular 'tool' for a single call", () => {
    const { getByRole } = render(() => (
      <CommittedToolCalls toolCalls={[{ name: "read_file", ok: true }]} />
    ));
    expect(getByRole("button").textContent).toContain("1 tool");
  });
});
