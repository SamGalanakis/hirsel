import { cva, type VariantProps } from "class-variance-authority";
import { omit } from "solid-js";
import { type ComponentProps } from "@solidjs/web";
import { cn } from "@/lib/utils";

const badgeVariants = cva(
  "group/badge z-badge inline-flex w-fit shrink-0 items-center justify-center overflow-hidden whitespace-nowrap transition-colors focus-visible:border-ring focus-visible:ring-[3px] focus-visible:ring-ring/50 aria-invalid:border-destructive aria-invalid:ring-destructive/20 dark:aria-invalid:ring-destructive/40 [&>svg]:pointer-events-none",
  {
    variants: {
      variant: {
        default: "z-badge-variant-default",
        secondary: "z-badge-variant-secondary",
        destructive: "z-badge-variant-destructive",
        outline: "z-badge-variant-outline",
        ghost: "z-badge-variant-ghost",
        link: "z-badge-variant-link",
      },
    },
    defaultVariants: {
      variant: "default",
    },
  },
);

type BadgeProps = ComponentProps<"span"> & VariantProps<typeof badgeVariants>;

const Badge = (props: BadgeProps) => {
  const local = props;
  const others = omit(props, "class", "variant");
  return (
    <span class={cn(badgeVariants({ variant: local.variant }), local.class)}
      data-slot="badge" data-variant={local.variant} {...others} />
  );
};

export { Badge, badgeVariants };
