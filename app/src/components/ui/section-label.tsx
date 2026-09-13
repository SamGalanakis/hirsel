import { Dynamic, type JSX } from "@solidjs/web";
import { omit } from "solid-js";

/** The one meta label over a group — a form field, a run's steps, a band of
 * the inventory, a section of the queue, a group of commands. DESIGN.md sets
 * it in sentence case at `text-meta`, medium, muted, with the ramp's own
 * tracking: the same label Thread Info already uses for its facts, so no
 * surface shouts NAME or STEPS over a table of plain ones. Tone and spacing
 * are the caller's (`class`); the type is not. */
export const sectionLabelClass = "text-meta font-medium text-muted-foreground";
/** The label's ink: muted by default; attention where it names what waits
 * on the Owner. A named tone, not a colour class, so the type and its ink
 * are composed here and never merged by a class-sorting helper. */
const TONE = { muted: "text-muted-foreground", attention: "text-status-attention" } as const;

type SectionLabelProps = JSX.HTMLAttributes<HTMLElement> & {
  /** The element the label is: a heading where it heads a landmark, a `p` or
   * `span` where it names a control the element beside it already labels. */
  as?: "p" | "span" | "h2" | "h3" | "li" | "div";
  tone?: keyof typeof TONE;
};

export function SectionLabel(props: SectionLabelProps) {
  const others = omit(props, "as", "class", "tone");
  return <Dynamic component={props.as ?? "p"} data-slot="section-label" class={`text-meta font-medium ${TONE[props.tone ?? "muted"]} ${props.class ?? ""}`} {...others} />;
}
