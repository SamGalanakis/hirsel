import { createEffect, createSignal, type JSX, Show } from "solid-js";
import { cn } from "@/lib/utils";
import type { Home } from "../store/store";
import { state } from "../store/store";

// The phone single-column home surface swap (craft wave). Below `rail` the phone
// shows exactly ONE of Feed (the event scroller) or Chat at a time, keyed by
// `state.home`. Switching between them was a hard cut; now it is a short
// horizontal cross-slide that reuses the sheet-slide vocabulary (tw-animate-css):
// going to Chat, Chat enters from the RIGHT; going back, Feed enters from the
// LEFT — the direction encodes the two surfaces' left/right relationship so the
// move reads as a lateral slide rather than a blink.
//
// Only ONE surface is ever mounted (the `<Show>` swaps them, and the two
// surfaces are passed as lazy factories so the hidden one is never instantiated),
// so this never double-mounts the scroller's snap/focus machinery or Chat's
// state — the incoming surface simply plays a 200ms enter as it takes the column.
// `motion-safe:` gates the whole thing, so reduced-motion is an instant swap.

/** The enter classes for whichever surface is taking the column. `animated` is
 * false on the very first paint (a fresh load is not a navigation), so the app
 * opens without a slide; every later `home` change animates. */
export function homeEnterClass(home: Home, animated: boolean): string {
  if (!animated) return "";
  const dir = home === "chat" ? "slide-in-from-right-4" : "slide-in-from-left-4";
  return `motion-safe:animate-in motion-safe:fade-in motion-safe:${dir} motion-safe:duration-200`;
}

export function PhoneHome(props: { feed: () => JSX.Element; chat: () => JSX.Element }): JSX.Element {
  // Gate the slide to genuine navigations: the first `home` read (mount) primes
  // the flag without animating; every change after that animates.
  const [animated, setAnimated] = createSignal(false);
  let primed = false;
  createEffect(() => {
    void state.home;
    if (!primed) {
      primed = true;
      return;
    }
    setAnimated(true);
  });

  return (
    <main class="flex min-h-0 flex-1 flex-col overflow-hidden">
      <Show
        when={state.home === "chat"}
        fallback={
          <div
            data-slot="phone-surface"
            data-surface="feed"
            class={cn("flex min-h-0 flex-1 flex-col", homeEnterClass("queue", animated()))}
          >
            {props.feed()}
          </div>
        }
      >
        <div
          data-slot="phone-surface"
          data-surface="chat"
          class={cn("flex min-h-0 flex-1 flex-col", homeEnterClass("chat", animated()))}
        >
          {props.chat()}
        </div>
      </Show>
    </main>
  );
}
