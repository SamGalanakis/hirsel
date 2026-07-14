import type { JSX } from "solid-js";

/**
 * The hirsel mark — the owner-supplied isometric cube. Its four faces are drawn
 * from theme-aware CSS tokens (`--brand-cube-*`): the brand's resting near-whites
 * on the dark canvas, ink-ish values on the light canvas. So the silhouette holds
 * on BOTH schemes on its own — no defensive contrast chip needed (the retired
 * `ring` prop). The geometry is kept verbatim.
 *
 * It is always decorative here: every place it renders sits beside a "hirsel"
 * wordmark or a `title`/`aria-label`-carrying tile, so the SVG itself is
 * `aria-hidden` to avoid a duplicate screen-reader announcement.
 */
export function BrandMark(props: {
  /** Rendered edge length in px. Default 24. */
  size?: number;
  class?: string;
}): JSX.Element {
  const size = () => props.size ?? 24;
  return (
    <span class="inline-flex items-center justify-center">
      <svg
        xmlns="http://www.w3.org/2000/svg"
        viewBox="18 8 64 64"
        width={size()}
        height={size()}
        aria-hidden="true"
        class={props.class}
      >
        <g transform="translate(50, 70)">
          <polygon points="0,0 30,-15 30,-45 0,-30" style={{ fill: "var(--brand-cube-right)" }} />
          <polygon points="0,0 -30,-15 -30,-45 0,-30" style={{ fill: "var(--brand-cube-left)" }} />
          <polygon points="0,-30 30,-45 0,-60 -30,-45" style={{ fill: "var(--brand-cube-top)" }} />
          <polygon points="-10,-10 -5,-15 -5,-25 -10,-20" style={{ fill: "var(--brand-cube-facet)" }} />
        </g>
      </svg>
    </span>
  );
}
