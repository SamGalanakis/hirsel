import { render } from "@solidjs/testing-library";
import { describe, expect, it, vi } from "vitest";
import { PaneHeader } from "./PaneHeader";

// The shared right-region header (spec item 1): one datum, one title token, and
// a trailing × close on every DISMISSIBLE pane (Canvas / Processes / Settings /
// Canvas) with a consistent focus-visible ring — while the standing task world
// wears the SAME header sans × (it is the default, not a dismissible inspector)
// and carries its count badge instead. These pin the close parity + the
// no-close-on-home contract the four inspectors rely on.
describe("PaneHeader: one slot, close parity", () => {
  it("gives a dismissible pane a labelled trailing × with the sibling focus ring", async () => {
    const onClose = vi.fn();
    const { getByLabelText, getByText } = render(() => (
      <PaneHeader
        icon={<span data-slot="icon" />}
        title="Canvas"
        onClose={onClose}
        closeLabel="Close Canvas"
      />
    ));
    const title = getByText("Canvas");
    // One title token — text-sm font-medium, no second scale.
    expect(title.className).toContain("text-sm");
    expect(title.className).toContain("font-medium");

    const close = getByLabelText("Close Canvas");
    close.click();
    expect(onClose).toHaveBeenCalledTimes(1);
    // Same focus-visible ring as the sibling controls (spec item 7).
    expect(close.className).toContain("focus-visible:ring-2");
    expect(close.className).toContain("focus-visible:ring-ring/50");
  });

  it("renders the resting Pings home with NO close and its badge instead", async () => {
    const { queryByLabelText, container } = render(() => (
      <PaneHeader
        icon={<span data-slot="icon" />}
        title="Pings"
        badge={<span data-slot="pings-rail-badge">3</span>}
      />
    ));
    // The home pane is not dismissible — no × to fat-finger.
    expect(queryByLabelText(/Close/)).toBeNull();
    expect(container.querySelector('[data-slot="pings-rail-badge"]')?.textContent).toBe("3");
    // Same fixed h-14 datum as every other pane AND as the task-world header,
    // so summoning a pane never jogs the content underneath it.
    expect((container.firstChild as HTMLElement).className).toContain("h-14");
  });

  it("is the one header at every width: sticky, safe-area padded, close-not-back", () => {
    const onClose = vi.fn();
    const { container, getByLabelText, queryByText } = render(() => (
      <PaneHeader
        icon={<span data-slot="icon" />}
        title="Processes"
        onClose={onClose}
        closeLabel="Close Processes"
      />
    ));
    const bar = container.firstChild as HTMLElement;
    expect(bar.className).toContain("sticky");
    expect(bar.className).toContain("top-0");
    // The notch is cleared by padding ON TOP of the datum, not out of it.
    expect(bar.className).toContain("pt-[env(safe-area-inset-top)]");
    expect(bar.className).toContain("box-content");

    // hirsel has no navigation stack: the ONLY exit is a close, never a back
    // chevron and never the word "Tasks".
    expect(queryByText("Tasks")).toBeNull();
    const close = getByLabelText("Close Processes");
    // Thumb-grade at both widths (PRODUCT: phone targets at least 44px).
    expect(close.className).toContain("[@media(pointer:coarse)]:size-11");
    close.click();
    expect(onClose).toHaveBeenCalledTimes(1);
  });
});
