/** The ONE inline-code treatment, shared by prose (Markdown's `inlineCode`
 * nodes) and by the event tier's backtick spans (views/nodes.tsx). It used to
 * be declared twice with two different sizes — 0.86em in prose, 0.92em in
 * events — so the same command name changed size when it moved between a reply
 * and a work row.
 *
 * Modelled on t3code's `.chat-markdown :not(pre) > code`: a hairline outline
 * over a quiet fill, not a heavy filled block. `bg-muted/70` with `px-1 py-0.5`
 * painted every command, path and identifier as a solid slab, so a sentence
 * naming three files read as three buttons; the outline says "this is literal"
 * without competing with the prose. The size is `em`-relative on purpose: it
 * lands on t3code's absolute 12px inside 14px reading type and still scales
 * down where code appears inside a meta line.
 */
export const inlineCodeClass =
  "rounded-md border border-border/60 bg-muted/40 px-[0.35em] py-[0.1em] font-mono text-[0.86em]";
