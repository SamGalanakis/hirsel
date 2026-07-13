// Ported from shadcn/ui `popover` (Radix) to SolidJS on @kobalte/core's popover,
// following the same Zaidan / react-to-solid conventions as dropdown-menu.tsx.
// Only Root, Trigger, and Portal+Content are ported (all the header model chip
// needs). Unstyled Kobalte primitives + Tailwind theme tokens.
import * as PopoverPrimitive from "@kobalte/core/popover";
import type { PolymorphicProps } from "@kobalte/core/polymorphic";
import { splitProps, type ValidComponent } from "solid-js";
import { cn } from "@/lib/utils";

const Popover = PopoverPrimitive.Root;
const PopoverTrigger = PopoverPrimitive.Trigger;

type PopoverContentProps<T extends ValidComponent = "div"> = PolymorphicProps<
  T,
  PopoverPrimitive.PopoverContentProps<T>
> & { class?: string };

const PopoverContent = <T extends ValidComponent = "div">(props: PopoverContentProps<T>) => {
  const [local, others] = splitProps(props as PopoverContentProps, ["class"]);
  return (
    <PopoverPrimitive.Portal>
      <PopoverPrimitive.Content
        class={cn(
          "z-50 min-w-[10rem] rounded-lg border border-border bg-popover p-1 text-popover-foreground shadow-md outline-none",
          "data-[expanded]:animate-in data-[closed]:animate-out data-[closed]:fade-out-0 data-[expanded]:fade-in-0 data-[expanded]:zoom-in-95",
          "origin-[var(--kb-popover-content-transform-origin)]",
          local.class,
        )}
        data-slot="popover-content"
        {...others}
      />
    </PopoverPrimitive.Portal>
  );
};

export { Popover, PopoverTrigger, PopoverContent };
