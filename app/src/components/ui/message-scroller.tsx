import { ArrowDown } from "lucide-solid";
import {
  type Accessor,
  type ComponentProps,
  createContext,
  createEffect,
  createMemo,
  createSignal,
  type JSX,
  mergeProps,
  onCleanup,
  Show,
  splitProps,
  useContext,
} from "solid-js";
import { cn } from "@/lib/utils";
import { Button, type ButtonProps } from "@/components/ui/button";

// SolidJS port of the shadcn June-2026 "MessageScroller" chat component
// (https://ui.shadcn.com/docs/changelog/2026-06-chat-components). The React
// registry item is a thin wrapper over the headless `@shadcn/react/message-
// scroller` primitive + its `useMessageScroller` / `...Scrollable` /
// `...Visibility` hooks; there is no Kobalte/Corvu equivalent, so the scroll
// behaviour is reimplemented here while preserving the part surface, the
// data-slot contract, and the hook API.
//
// Owns scroll logic ONLY (per the source: "without controlling messages, AI
// state, transport, or persistence"): stick-to-bottom on growth/stream, restore
// to newest on mount, jump-to-message, a scroll-to-end control, and per-item
// visibility + an unseen-below count. This replaces hirsel's hand-rolled
// near-bottom autoscroll effect.

const EDGE_THRESHOLD_PX = 32;

/** Pure edge math, exported so the near-bottom autoscroll decision is unit
 * testable without a DOM. `atEnd` drives stick-to-bottom; `atStart` drives the
 * (optional) scroll-to-start control. */
export function computeEdges(
  scrollTop: number,
  scrollHeight: number,
  clientHeight: number,
  threshold = EDGE_THRESHOLD_PX,
): { atStart: boolean; atEnd: boolean; distanceFromBottom: number } {
  const distanceFromBottom = scrollHeight - scrollTop - clientHeight;
  return {
    atStart: scrollTop <= threshold,
    // A non-scrollable viewport (content shorter than the viewport) is,
    // definitionally, already at the end.
    atEnd: distanceFromBottom <= threshold,
    distanceFromBottom,
  };
}

/** True when the viewport is close enough to the bottom that a newly appended
 * message should keep it pinned there (stick-to-bottom). */
export function isNearBottom(
  el: Pick<HTMLElement, "scrollTop" | "scrollHeight" | "clientHeight">,
  threshold = EDGE_THRESHOLD_PX,
): boolean {
  return computeEdges(el.scrollTop, el.scrollHeight, el.clientHeight, threshold).atEnd;
}

interface MessageScrollerContextValue {
  setViewport: (el: HTMLDivElement) => void;
  setContent: (el: HTMLDivElement) => void;
  registerItem: (el: HTMLElement) => () => void;
  atStart: Accessor<boolean>;
  atEnd: Accessor<boolean>;
  autoscrolling: Accessor<boolean>;
  unseenCount: Accessor<number>;
  scrollToEnd: (behavior?: ScrollBehavior) => void;
  scrollToStart: (behavior?: ScrollBehavior) => void;
  scrollToElement: (el: HTMLElement, opts?: ScrollIntoViewOptions) => void;
  /** Run `mutate` (which prepends older rows) while holding the viewport visually
   * still: the content the user is looking at stays put instead of jumping down
   * as height is added above it (D10 "load older"). */
  preserveOnPrepend: (mutate: () => void) => void;
}

const MessageScrollerContext = createContext<MessageScrollerContextValue>();

function useScrollerCtx(who: string): MessageScrollerContextValue {
  const ctx = useContext(MessageScrollerContext);
  if (!ctx) throw new Error(`${who} must be used within <MessageScroller>`);
  return ctx;
}

/** The full context (viewport control + state). */
export function useMessageScroller(): MessageScrollerContextValue {
  return useScrollerCtx("useMessageScroller");
}

/** Edge state for scroll controls: { atStart, atEnd }. */
export function useMessageScrollerScrollable(): {
  atStart: Accessor<boolean>;
  atEnd: Accessor<boolean>;
} {
  const ctx = useScrollerCtx("useMessageScrollerScrollable");
  return { atStart: ctx.atStart, atEnd: ctx.atEnd };
}

/** Visibility state: the count of items below the fold (unseen). */
export function useMessageScrollerVisibility(): { unseenCount: Accessor<number> } {
  const ctx = useScrollerCtx("useMessageScrollerVisibility");
  return { unseenCount: ctx.unseenCount };
}

function MessageScrollerProvider(props: { children: JSX.Element }) {
  const [viewport, setViewport] = createSignal<HTMLDivElement>();
  const [content, setContent] = createSignal<HTMLDivElement>();
  const [atStart, setAtStart] = createSignal(true);
  const [atEnd, setAtEnd] = createSignal(true);
  const [autoscrolling, setAutoscrolling] = createSignal(false);
  const [unseenCount, setUnseenCount] = createSignal(0);

  const items = new Set<HTMLElement>();
  // `stick` = keep the viewport pinned to the bottom as content grows. Starts
  // true so a freshly restored thread lands on the newest message.
  let stick = true;

  // The unseen scan is O(items) getBoundingClientRect reads (a forced reflow
  // each) — far too heavy to run on every scroll tick (D10). Coalesce all
  // requests within a frame into a single scan on the next animation frame.
  let unseenRaf = 0;
  const recomputeUnseenNow = () => {
    const vp = viewport();
    if (!vp) return;
    const bottom = vp.getBoundingClientRect().bottom;
    let count = 0;
    for (const el of items) {
      // An item whose top sits at/below the viewport's bottom edge is below the
      // fold — i.e. not yet seen. (Items above the fold have top < bottom.)
      if (el.getBoundingClientRect().top >= bottom - 1) count += 1;
    }
    setUnseenCount(count);
  };
  const recomputeUnseen = () => {
    if (unseenRaf) return;
    unseenRaf = requestAnimationFrame(() => {
      unseenRaf = 0;
      recomputeUnseenNow();
    });
  };

  const measure = () => {
    const vp = viewport();
    if (!vp) return;
    const edges = computeEdges(vp.scrollTop, vp.scrollHeight, vp.clientHeight);
    setAtStart(edges.atStart);
    setAtEnd(edges.atEnd);
    recomputeUnseen();
  };

  const scrollToEnd = (behavior: ScrollBehavior = "smooth") => {
    const vp = viewport();
    if (!vp) return;
    stick = true;
    vp.scrollTo({ top: vp.scrollHeight, behavior });
  };

  const scrollToStart = (behavior: ScrollBehavior = "smooth") => {
    viewport()?.scrollTo({ top: 0, behavior });
  };

  const scrollToElement = (el: HTMLElement, opts?: ScrollIntoViewOptions) => {
    el.scrollIntoView({ behavior: "smooth", block: "center", ...opts });
  };

  const preserveOnPrepend = (mutate: () => void) => {
    const vp = viewport();
    if (!vp) {
      mutate();
      return;
    }
    // Record height/position, add the older rows, then restore the same content
    // to the same place by pushing scrollTop down by exactly the height gained.
    const beforeHeight = vp.scrollHeight;
    const beforeTop = vp.scrollTop;
    mutate();
    requestAnimationFrame(() => {
      vp.scrollTop = beforeTop + (vp.scrollHeight - beforeHeight);
    });
  };

  const registerItem = (el: HTMLElement) => {
    items.add(el);
    recomputeUnseen();
    return () => {
      items.delete(el);
      recomputeUnseen();
    };
  };

  // Wire the scroll listener + growth observer once both the viewport and its
  // content element exist. Re-runs (with cleanup) if either ref changes.
  createEffect(() => {
    const vp = viewport();
    const inner = content();
    if (!vp || !inner) return;

    const onScroll = () => {
      const edges = computeEdges(vp.scrollTop, vp.scrollHeight, vp.clientHeight);
      setAtStart(edges.atStart);
      setAtEnd(edges.atEnd);
      // The user's own scroll decides whether we keep sticking: releasing near
      // the bottom re-arms stick; scrolling up disengages it.
      stick = edges.atEnd;
      recomputeUnseen();
    };
    vp.addEventListener("scroll", onScroll, { passive: true });

    // Growth (new message, image load, streamed tokens) pins to the bottom when
    // still sticking — this is the autoscroll, minus any hand-rolled length
    // tracking. Instant (not smooth) so streaming doesn't fight the animation.
    const ro = new ResizeObserver(() => {
      if (stick && !autoscrolling()) {
        setAutoscrolling(true);
        vp.scrollTop = vp.scrollHeight;
        // Clear the flag on the next frame so a subsequent user scroll is not
        // mistaken for our own programmatic one.
        requestAnimationFrame(() => setAutoscrolling(false));
      }
      measure();
    });
    ro.observe(inner);

    // Restore to the newest message on first wire-up.
    vp.scrollTop = vp.scrollHeight;
    measure();

    onCleanup(() => {
      vp.removeEventListener("scroll", onScroll);
      ro.disconnect();
    });
  });

  const value: MessageScrollerContextValue = {
    setViewport,
    setContent,
    registerItem,
    atStart,
    atEnd,
    autoscrolling,
    unseenCount,
    scrollToEnd,
    scrollToStart,
    scrollToElement,
    preserveOnPrepend,
  };

  onCleanup(() => {
    if (unseenRaf) cancelAnimationFrame(unseenRaf);
  });

  return (
    <MessageScrollerContext.Provider value={value}>
      {props.children}
    </MessageScrollerContext.Provider>
  );
}

type MessageScrollerProps = ComponentProps<"div">;

/** Root: provides the scroll context and the flex column shell. */
const MessageScroller = (props: MessageScrollerProps) => {
  const [local, others] = splitProps(props, ["class", "children"]);
  return (
    <MessageScrollerProvider>
      <div
        data-slot="message-scroller"
        class={cn(
          "group/message-scroller relative flex size-full min-h-0 flex-col overflow-hidden",
          local.class,
        )}
        {...others}
      >
        {local.children}
      </div>
    </MessageScrollerProvider>
  );
};

type MessageScrollerViewportProps = ComponentProps<"div">;

/** The scrollable element. Owns the scroll listener via the context. */
const MessageScrollerViewport = (props: MessageScrollerViewportProps) => {
  const ctx = useMessageScroller();
  const [local, others] = splitProps(props, ["class", "ref"]);
  return (
    <div
      data-slot="message-scroller-viewport"
      ref={(el) => {
        ctx.setViewport(el);
        if (typeof local.ref === "function") (local.ref as (e: HTMLDivElement) => void)(el);
      }}
      class={cn(
        "size-full min-h-0 min-w-0 scroll-fade-b overflow-y-auto overscroll-contain",
        local.class,
      )}
      {...others}
    />
  );
};

type MessageScrollerContentProps = ComponentProps<"div">;

/** Inner column whose growth drives the stick-to-bottom autoscroll. */
const MessageScrollerContent = (props: MessageScrollerContentProps) => {
  const ctx = useMessageScroller();
  const [local, others] = splitProps(props, ["class", "ref"]);
  return (
    <div
      data-slot="message-scroller-content"
      ref={(el) => {
        ctx.setContent(el);
        if (typeof local.ref === "function") (local.ref as (e: HTMLDivElement) => void)(el);
      }}
      class={cn("flex h-max min-h-full flex-col", local.class)}
      {...others}
    />
  );
};

type MessageScrollerItemProps = ComponentProps<"div"> & { scrollAnchor?: boolean };

/** A conversation row. Registers for visibility (unseen-below) tracking. */
const MessageScrollerItem = (props: MessageScrollerItemProps) => {
  const ctx = useMessageScroller();
  const merged = mergeProps({ scrollAnchor: false }, props);
  const [local, others] = splitProps(merged, ["class", "scrollAnchor", "ref"]);
  return (
    <div
      data-slot="message-scroller-item"
      data-scroll-anchor={local.scrollAnchor ? "" : undefined}
      ref={(el) => {
        const unregister = ctx.registerItem(el);
        onCleanup(unregister);
        if (typeof local.ref === "function") (local.ref as (e: HTMLDivElement) => void)(el);
      }}
      class={cn("min-w-0 shrink-0", local.class)}
      {...others}
    />
  );
};

type MessageScrollerButtonProps = ComponentProps<"button"> &
  Pick<ButtonProps, "variant" | "size"> & {
    direction?: "start" | "end";
  };

/** Floating scroll-to-edge control. `data-active` reflects whether the viewport
 * is away from that edge, so the caller can fade it in/out. Defaults to the
 * "scroll to end" (bottom) control used by the chat thread. */
const MessageScrollerButton = (props: MessageScrollerButtonProps) => {
  const ctx = useMessageScroller();
  const merged = mergeProps(
    { direction: "end", variant: "secondary", size: "icon-sm" } as const,
    props,
  );
  const [local, others] = splitProps(merged, [
    "direction",
    "variant",
    "size",
    "class",
    "children",
    "onClick",
  ]);
  const active = createMemo(() => (local.direction === "end" ? !ctx.atEnd() : !ctx.atStart()));
  return (
    <Button
      data-slot="message-scroller-button"
      data-direction={local.direction}
      data-active={active()}
      variant={local.variant}
      size={local.size}
      class={cn(
        "absolute inset-x-0 mx-auto w-fit rounded-full border border-border bg-background text-foreground shadow-sm transition-[opacity,translate,scale] duration-200 hover:bg-muted data-[active=false]:pointer-events-none data-[active=false]:scale-95 data-[active=false]:opacity-0 data-[active=true]:scale-100 data-[active=true]:opacity-100 data-[direction=end]:bottom-3 data-[direction=start]:top-3",
        local.class,
      )}
      onClick={(e: MouseEvent & { currentTarget: HTMLButtonElement; target: Element }) => {
        if (local.direction === "end") ctx.scrollToEnd();
        else ctx.scrollToStart();
        if (typeof local.onClick === "function")
          (local.onClick as JSX.EventHandler<HTMLButtonElement, MouseEvent>)(e);
      }}
      {...others}
    >
      <Show
        when={local.children}
        fallback={
          <>
            <ArrowDown class="size-4" />
            <span class="sr-only">
              {local.direction === "end" ? "Scroll to end" : "Scroll to start"}
            </span>
          </>
        }
      >
        {local.children}
      </Show>
    </Button>
  );
};

export {
  MessageScroller,
  MessageScrollerButton,
  MessageScrollerContent,
  MessageScrollerItem,
  MessageScrollerProvider,
  MessageScrollerViewport,
};
