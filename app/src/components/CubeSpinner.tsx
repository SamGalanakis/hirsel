import type { JSX } from "@solidjs/web";

/**
 * The working indicator: the hirsel cube itself, tumbling.
 *
 * It is the same isometric geometry as {@link BrandMark}, drawn from the same
 * `--brand-cube-*` tokens. The motion is nothing but the shading rotating
 * around the solid — each face hands its token to the next on a 1.5s cycle, so
 * at every instant the three faces still hold the three distinct values and the
 * silhouette never stops reading as a cube. No spin, no blur, no glow, no
 * colour the brand mark does not already own; a quarter-turn eases over, rests,
 * and eases again, which is what "deliberate work" looks like.
 *
 * The animation is CSS on `[data-slot="cube-spinner"]` (see styles.css), never
 * a JS timer, so a hundred live cards cost one compositor cycle. Under
 * `prefers-reduced-motion` the faces settle back onto their own tokens and it
 * is simply the resting mark.
 *
 * Decorative: every call site already carries the spoken status beside it
 * (the run card's `role="status"` line, a pill's label), so the SVG is
 * `aria-hidden` and never announces itself twice.
 */
export function CubeSpinner(props: {
  /** Rendered edge length in px. Default 14 — the size of the meta line it sits in. */
  size?: number;
  /** Freeze the tumble without losing the mark (used while the socket is down). */
  paused?: boolean;
  class?: string;
}): JSX.Element {
  const size = () => props.size ?? 14;
  return (
    <span
      data-slot="cube-spinner"
      data-paused={props.paused ? "" : undefined}
      class={`inline-flex shrink-0 items-center justify-center ${props.class ?? ""}`}
      aria-hidden="true"
    >
      <svg xmlns="http://www.w3.org/2000/svg" viewBox="18 8 64 64" width={size()} height={size()}>
        <g transform="translate(50, 70)">
          <polygon data-face="right" style={{ "--cube-face": "var(--brand-cube-right)" }} points="0,0 30,-15 30,-45 0,-30" />
          <polygon data-face="left" style={{ "--cube-face": "var(--brand-cube-left)" }} points="0,0 -30,-15 -30,-45 0,-30" />
          <polygon data-face="top" style={{ "--cube-face": "var(--brand-cube-top)" }} points="0,-30 30,-45 0,-60 -30,-45" />
          <polygon points="-10,-10 -5,-15 -5,-25 -10,-20" style={{ fill: "var(--brand-cube-facet)" }} />
        </g>
      </svg>
    </span>
  );
}
