// Cross-surface focus handoff between the two composers that can be on screen
// at once (main Chat + an open Side Chat). Exactly one composer should hold
// focus at a time (design: "visible focus cues, one composer focused"), so
// leaving/closing a side chat returns focus to the main composer where the
// Owner's next keystroke would go. DOM-query based (rather than a passed ref)
// so it works across the component boundary without threading a ref through
// ChatView → SideChatSheet. Deferred a microtask so it runs after the sheet's
// own teardown has settled.

export function focusMainComposer(): void {
  queueMicrotask(() => {
    document.querySelector<HTMLTextAreaElement>('[data-composer="main"]')?.focus();
  });
}
