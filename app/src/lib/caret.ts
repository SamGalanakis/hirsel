// Where the caret is, in the textarea's own box.
//
// A textarea gives no geometry for a character offset, so the standard trick is
// the only one: mirror the field into an off-screen div with identical text
// metrics, put a marker span at the offset, and read the marker's position. It
// is used for exactly one thing here — anchoring the Task-ref picker under the
// `#` the Owner is typing — so it stays deliberately small and total: any
// environment that cannot measure (jsdom, an unattached node) gets `{x:0, y:0}`,
// and the picker simply anchors to the start of the field.

/** The properties that decide where a glyph lands. Copied onto the mirror so the
 * measurement is the real one, not an approximation. */
const MIRRORED = [
  "boxSizing",
  "width",
  "paddingTop",
  "paddingRight",
  "paddingBottom",
  "paddingLeft",
  "borderTopWidth",
  "borderRightWidth",
  "borderBottomWidth",
  "borderLeftWidth",
  "fontFamily",
  "fontSize",
  "fontWeight",
  "fontStyle",
  "letterSpacing",
  "lineHeight",
  "textIndent",
  "textTransform",
  "whiteSpace",
  "wordSpacing",
  "wordBreak",
  "overflowWrap",
] as const;

export interface CaretPoint {
  /** Offset from the field's left edge, in CSS pixels. */
  x: number;
  /** Offset from the field's top edge to the TOP of the caret's line. */
  y: number;
  /** The line box height at the caret. */
  lineHeight: number;
}

/** The caret's position within `el`, for the character offset `index`. */
export function caretPoint(el: HTMLTextAreaElement, index: number): CaretPoint {
  const fallback: CaretPoint = { x: 0, y: 0, lineHeight: 0 };
  if (typeof window === "undefined" || !window.getComputedStyle || !el.isConnected) {
    return fallback;
  }
  const computed = window.getComputedStyle(el);
  const mirror = document.createElement("div");
  const style = mirror.style;
  style.position = "absolute";
  style.visibility = "hidden";
  style.top = "0";
  style.left = "-9999px";
  style.whiteSpace = "pre-wrap";
  style.overflowWrap = "break-word";
  for (const key of MIRRORED) {
    const value = computed[key];
    if (typeof value === "string") style.setProperty(hyphenate(key), value);
  }

  mirror.textContent = el.value.slice(0, index);
  const marker = document.createElement("span");
  // A zero-width text node collapses to no box in some engines; a real glyph
  // measures reliably and is never seen (the mirror is hidden).
  marker.textContent = el.value.slice(index) || ".";
  mirror.appendChild(marker);
  document.body.appendChild(mirror);
  try {
    const x = marker.offsetLeft;
    const y = marker.offsetTop;
    const lineHeight = marker.offsetHeight;
    if (!Number.isFinite(x) || !Number.isFinite(y)) return fallback;
    return { x: x - el.scrollLeft, y: y - el.scrollTop, lineHeight };
  } catch {
    return fallback;
  } finally {
    mirror.remove();
  }
}

function hyphenate(key: string): string {
  return key.replace(/[A-Z]/g, (c) => `-${c.toLowerCase()}`);
}
