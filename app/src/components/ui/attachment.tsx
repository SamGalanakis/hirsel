import { cva, type VariantProps } from "class-variance-authority";
import type { ComponentProps } from "solid-js";
import { mergeProps, splitProps } from "solid-js";
import { cn } from "@/lib/utils";
import { Button, type ButtonProps } from "@/components/ui/button";

// Ported from the shadcn June-2026 conversation "Attachment" component via lashapp
// SolidJS donor kit (itself a react-to-solid port). Preserves the data-slot /
// data-state contract the vega `z-attachment-*` tokens key off of.

const attachmentVariants = cva(
  "z-attachment group/attachment relative flex max-w-full min-w-0 shrink-0 flex-wrap border bg-card text-card-foreground transition-colors has-[>a,>button]:hover:bg-muted/50 data-[state=error]:border-destructive/30 data-[state=idle]:border-dashed",
  {
    variants: {
      size: {
        default: "z-attachment-size-default",
        sm: "z-attachment-size-sm",
        xs: "z-attachment-size-xs",
      },
      orientation: {
        horizontal: "z-attachment-orientation-horizontal items-center",
        vertical: "z-attachment-orientation-vertical flex-col",
      },
    },
  },
);

type AttachmentState = "idle" | "uploading" | "processing" | "error" | "done";

type AttachmentProps = ComponentProps<"div"> &
  VariantProps<typeof attachmentVariants> & { state?: AttachmentState };

const Attachment = (props: AttachmentProps) => {
  const mergedProps = mergeProps(
    { state: "done", size: "default", orientation: "horizontal" } as const,
    props,
  );
  const [local, others] = splitProps(mergedProps, ["class", "state", "size", "orientation"]);
  return (
    <div
      data-slot="attachment"
      data-state={local.state}
      data-size={local.size}
      data-orientation={local.orientation}
      class={cn(
        attachmentVariants({ size: local.size, orientation: local.orientation }),
        local.class,
      )}
      {...others}
    />
  );
};

const attachmentMediaVariants = cva(
  "z-attachment-media relative flex aspect-square shrink-0 items-center justify-center overflow-hidden group-data-[state=error]/attachment:bg-destructive/10 group-data-[state=error]/attachment:text-destructive [&_svg]:pointer-events-none",
  {
    variants: {
      variant: {
        icon: "z-attachment-media-variant-icon",
        image:
          "z-attachment-media-variant-image *:[img]:aspect-square *:[img]:w-full *:[img]:object-cover",
      },
    },
    defaultVariants: {
      variant: "icon",
    },
  },
);

type AttachmentMediaProps = ComponentProps<"div"> & VariantProps<typeof attachmentMediaVariants>;

const AttachmentMedia = (props: AttachmentMediaProps) => {
  const mergedProps = mergeProps({ variant: "icon" } as const, props);
  const [local, others] = splitProps(mergedProps, ["class", "variant"]);
  return (
    <div
      data-slot="attachment-media"
      data-variant={local.variant}
      class={cn(attachmentMediaVariants({ variant: local.variant }), local.class)}
      {...others}
    />
  );
};

type AttachmentContentProps = ComponentProps<"div">;

const AttachmentContent = (props: AttachmentContentProps) => {
  const [local, others] = splitProps(props, ["class"]);
  return (
    <div
      data-slot="attachment-content"
      class={cn("z-attachment-content max-w-full min-w-0 flex-1", local.class)}
      {...others}
    />
  );
};

type AttachmentTitleProps = ComponentProps<"span">;

const AttachmentTitle = (props: AttachmentTitleProps) => {
  const [local, others] = splitProps(props, ["class"]);
  return (
    <span
      data-slot="attachment-title"
      class={cn(
        "z-attachment-title block max-w-full min-w-0 truncate group-data-[state=processing]/attachment:shimmer group-data-[state=uploading]/attachment:shimmer",
        local.class,
      )}
      {...others}
    />
  );
};

type AttachmentDescriptionProps = ComponentProps<"span">;

const AttachmentDescription = (props: AttachmentDescriptionProps) => {
  const [local, others] = splitProps(props, ["class"]);
  return (
    <span
      data-slot="attachment-description"
      class={cn(
        "z-attachment-description block max-w-full min-w-0 truncate text-muted-foreground group-data-[state=error]/attachment:text-destructive/80",
        local.class,
      )}
      {...others}
    />
  );
};

type AttachmentActionsProps = ComponentProps<"div">;

const AttachmentActions = (props: AttachmentActionsProps) => {
  const [local, others] = splitProps(props, ["class"]);
  return (
    <div
      data-slot="attachment-actions"
      class={cn("z-attachment-actions flex shrink-0 items-center", local.class)}
      {...others}
    />
  );
};

type AttachmentActionProps = ButtonProps;

const AttachmentAction = (props: AttachmentActionProps) => {
  const mergedProps = mergeProps({ variant: "ghost", size: "icon-xs" } as const, props);
  const [local, others] = splitProps(mergedProps as AttachmentActionProps, ["class"]);
  return (
    <Button
      data-slot="attachment-action"
      class={cn("z-attachment-action", local.class)}
      {...others}
    />
  );
};

type AttachmentGroupProps = ComponentProps<"div">;

const AttachmentGroup = (props: AttachmentGroupProps) => {
  const [local, others] = splitProps(props, ["class"]);
  return (
    <div
      data-slot="attachment-group"
      class={cn(
        "z-attachment-group flex min-w-0 scroll-fade-x snap-x snap-mandatory overflow-x-auto overscroll-x-contain no-scrollbar *:data-[slot=attachment]:flex-none *:data-[slot=attachment]:snap-start",
        local.class,
      )}
      {...others}
    />
  );
};

export {
  Attachment,
  AttachmentAction,
  AttachmentActions,
  AttachmentContent,
  AttachmentDescription,
  AttachmentGroup,
  AttachmentMedia,
  AttachmentTitle,
  type AttachmentState,
};
