import { cva, type VariantProps } from "class-variance-authority";
import type { ComponentProps } from "solid-js";
import { mergeProps, splitProps } from "solid-js";
import { cn } from "@/lib/utils";

// Ported from the shadcn June-2026 chat "Marker" component via the lashapp
// SolidJS donor kit. Renders status / system rows: streaming state, tool
// activity, date breaks, and labeled separators. Pairs with the `shimmer`
// utility for live "Thinking…" text.

const markerVariants = cva("z-marker group/marker relative flex w-full items-center", {
  variants: {
    variant: {
      default: "z-marker-variant-default",
      separator: "z-marker-variant-separator",
      border: "z-marker-variant-border",
    },
  },
});

type MarkerProps = ComponentProps<"div"> & VariantProps<typeof markerVariants>;

const Marker = (props: MarkerProps) => {
  const mergedProps = mergeProps({ variant: "default" } as const, props);
  const [local, others] = splitProps(mergedProps, ["class", "variant"]);
  return (
    <div
      data-slot="marker"
      data-variant={local.variant}
      class={cn(markerVariants({ variant: local.variant }), local.class)}
      {...others}
    />
  );
};

type MarkerIconProps = ComponentProps<"span">;

const MarkerIcon = (props: MarkerIconProps) => {
  const [local, others] = splitProps(props, ["class"]);
  return (
    <span
      data-slot="marker-icon"
      aria-hidden="true"
      class={cn("z-marker-icon shrink-0", local.class)}
      {...others}
    />
  );
};

type MarkerContentProps = ComponentProps<"span">;

const MarkerContent = (props: MarkerContentProps) => {
  const [local, others] = splitProps(props, ["class"]);
  return (
    <span
      data-slot="marker-content"
      class={cn("z-marker-content min-w-0 wrap-break-word", local.class)}
      {...others}
    />
  );
};

export { Marker, MarkerContent, MarkerIcon, markerVariants };
