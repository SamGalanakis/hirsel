import { cva, type VariantProps } from "class-variance-authority";
import type { ComponentProps } from "solid-js";
import { mergeProps, splitProps } from "solid-js";
import { cn } from "@/lib/utils";

type BubbleGroupProps = ComponentProps<"div">;

const BubbleGroup = (props: BubbleGroupProps) => {
  const [local, others] = splitProps(props, ["class"]);
  return (
    <div
      data-slot="bubble-group"
      class={cn("z-bubble-group flex min-w-0 flex-col", local.class)}
      {...others}
    />
  );
};

const bubbleVariants = cva("z-bubble group/bubble relative flex w-fit min-w-0 flex-col", {
  variants: {
    variant: {
      default: "z-bubble-variant-default",
      secondary: "z-bubble-variant-secondary",
      muted: "z-bubble-variant-muted",
      tinted: "z-bubble-variant-tinted",
      outline: "z-bubble-variant-outline",
      ghost: "z-bubble-variant-ghost",
      destructive: "z-bubble-variant-destructive",
    },
  },
  defaultVariants: {
    variant: "default",
  },
});

type BubbleProps = ComponentProps<"div"> &
  VariantProps<typeof bubbleVariants> & { align?: "start" | "end" };

const Bubble = (props: BubbleProps) => {
  const mergedProps = mergeProps({ variant: "default", align: "start" } as const, props);
  const [local, others] = splitProps(mergedProps, ["class", "variant", "align"]);
  return (
    <div
      data-slot="bubble"
      data-variant={local.variant}
      data-align={local.align}
      class={cn(bubbleVariants({ variant: local.variant }), local.class)}
      {...others}
    />
  );
};

type BubbleContentProps = ComponentProps<"div">;

const BubbleContent = (props: BubbleContentProps) => {
  const [local, others] = splitProps(props, ["class"]);
  return (
    <div
      data-slot="bubble-content"
      class={cn(
        "z-bubble-content w-fit max-w-full min-w-0 overflow-hidden wrap-break-word [button]:text-left [button,a]:transition-colors",
        local.class,
      )}
      {...others}
    />
  );
};

export { Bubble, BubbleContent, BubbleGroup, bubbleVariants };
