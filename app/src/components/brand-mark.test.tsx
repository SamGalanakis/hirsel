import { render } from "@solidjs/testing-library";
import { describe, expect, it } from "vitest";
import { BrandMark } from "./BrandMark";

// Craft wave: the mark's faces are theme-aware tokens so the silhouette holds on
// both canvases WITHOUT the retired defensive `ring` chip.

describe("BrandMark — theme-aware token fills, no defensive chip", () => {
  it("fills every face from a --brand-cube-* token", () => {
    const { container } = render(() => <BrandMark size={20} />);
    const polys = Array.from(container.querySelectorAll("polygon"));
    expect(polys).toHaveLength(4);
    const styles = polys.map((p) => p.getAttribute("style") ?? "");
    expect(styles.some((s) => s.includes("var(--brand-cube-top)"))).toBe(true);
    expect(styles.some((s) => s.includes("var(--brand-cube-right)"))).toBe(true);
    expect(styles.some((s) => s.includes("var(--brand-cube-left)"))).toBe(true);
    expect(styles.some((s) => s.includes("var(--brand-cube-facet)"))).toBe(true);
    // No raw hard-coded near-white on the face any more (it lives in the token).
    for (const p of polys) expect(p.getAttribute("fill")).toBeNull();
  });

  it("wraps the mark without the retired contrast chip (no ring / bg-muted)", () => {
    const { container } = render(() => <BrandMark size={24} />);
    const wrapper = container.querySelector("span") as HTMLElement;
    expect(wrapper.className).not.toContain("ring");
    expect(wrapper.className).not.toContain("bg-muted");
    // The svg is still decorative (announced by the adjacent wordmark).
    expect(container.querySelector("svg")?.getAttribute("aria-hidden")).toBe("true");
  });
});
