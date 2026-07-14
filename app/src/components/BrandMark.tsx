import type { JSX } from "solid-js";

/**
 * The hirsel mark — the owner-supplied isometric cube (geometry + fills kept
 * verbatim). It is always decorative here: every place it renders sits beside a
 * "hirsel" wordmark or a `title`/`aria-label`-carrying tile, so the SVG itself is
 * `aria-hidden` to avoid a duplicate screen-reader announcement.
 *
 * The top and right faces are near-white (#ffffff / #f8fafc), so on a light/white
 * surface the mark washes out. Pass `ring` on light-theme surfaces to seat it in a
 * subtle `bg-muted` chip with a hairline border — the smallest honest fix that
 * keeps the silhouette legible without touching the mark's colours.
 */
export function BrandMark(props: {
  /** Rendered edge length in px. Default 24. */
  size?: number;
  /** Frame the mark in a muted contrast chip — use where it sits on a light surface. */
  ring?: boolean;
  class?: string;
}): JSX.Element {
  const size = () => props.size ?? 24;
  return (
    <span
      class="inline-flex items-center justify-center"
      classList={{
        "rounded-md bg-muted p-1 ring-1 ring-inset ring-border/60": props.ring,
      }}
    >
      <svg
        xmlns="http://www.w3.org/2000/svg"
        viewBox="18 8 64 64"
        width={size()}
        height={size()}
        aria-hidden="true"
        class={props.class}
      >
        <g transform="translate(50, 70)">
          <polygon points="0,0 30,-15 30,-45 0,-30" fill="#f8fafc" />
          <polygon points="0,0 -30,-15 -30,-45 0,-30" fill="#0f172a" />
          <polygon points="0,-30 30,-45 0,-60 -30,-45" fill="#ffffff" />
          <polygon points="-10,-10 -5,-15 -5,-25 -10,-20" fill="#94a3b8" />
        </g>
      </svg>
    </span>
  );
}
