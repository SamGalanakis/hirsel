// The small overflow menu uses native buttons with the ARIA menu keyboard
// contract. No framework-specific headless primitive owns focus behind it.
import { createContext, createSignal, onCleanup, onSettled, Show, omit, useContext } from "solid-js";
import { type ComponentProps, type JSX, Portal } from "@solidjs/web";
import { registerOverlayPresence } from "@/lib/focus";
import { cn } from "@/lib/utils";

interface MenuState {
  open: () => boolean;
  setOpen: (open: boolean) => void;
  trigger?: HTMLButtonElement;
  content?: HTMLDivElement;
  first: "first" | "last";
  gutter: number;
  close: (restore?: boolean) => void;
}
const MenuContext = createContext<MenuState>();
function menuContext() {
  const menu = useContext(MenuContext);
  if (!menu) throw new Error("Dropdown menu parts require DropdownMenu");
  return menu;
}
function menuItems(panel: HTMLElement) {
  return Array.from(panel.querySelectorAll<HTMLButtonElement>('[role="menuitem"]:not([disabled]), [role="menuitemradio"]:not([disabled])'));
}
function DropdownMenu(props: { children: JSX.Element; placement?: "bottom-end"; gutter?: number; onOpenChange?: (open: boolean) => void }) {
  const [open, setOpen] = createSignal(false);
  const menu: MenuState = {
    open, setOpen(value) { props.onOpenChange?.(value); setOpen(value); }, first: "first", gutter: props.gutter ?? 6,
    close(restore = true) {
      setOpen(false);
      if (restore) menu.trigger?.focus();
    },
  };
  return <MenuContext value={menu}>{props.children}</MenuContext>;
}
function DropdownMenuTrigger(props: ComponentProps<"button">) {
  const menu = menuContext();
  return <button type="button" {...props} ref={(element) => { menu.trigger = element; }}
    aria-haspopup="menu" aria-expanded={menu.open() ? "true" : "false"}
    onClick={() => { menu.first = "first"; menu.setOpen(!menu.open()); }}
    onKeyDown={(event) => {
      if (event.key !== "ArrowDown" && event.key !== "ArrowUp") return;
      event.preventDefault();
      menu.first = event.key === "ArrowUp" ? "last" : "first";
      menu.setOpen(true);
    }} />;
}
function DropdownMenuContent(props: ComponentProps<"div">) {
  const menu = menuContext();
  return <Show when={menu.open()}><Portal mount={menu.trigger?.closest("dialog") ?? undefined}><MenuPanel {...props} /></Portal></Show>;
}
function MenuPanel(props: ComponentProps<"div">) {
  const menu = menuContext();
  const local = props;
  const others = omit(props, "class");
  let panel!: HTMLDivElement;
  const [position, setPosition] = createSignal({left: "0px", top: "0px"});
  onCleanup(registerOverlayPresence());
  onSettled(() => {
    menu.content = panel;
    const place = () => {
      const trigger = menu.trigger?.getBoundingClientRect();
      if (!trigger) return;
      const height = panel.getBoundingClientRect().height;
      const width = panel.getBoundingClientRect().width;
      setPosition({
        left: `${Math.max(8, Math.min(trigger.right - width, window.innerWidth - width - 8))}px`,
        top: `${Math.max(8, Math.min(trigger.bottom + menu.gutter, window.innerHeight - height - 8))}px`,
      });
    };
    place();
    const items = menuItems(panel);
    (menu.first === "last" ? items.at(-1) : items[0])?.focus();
    const outside = (event: PointerEvent) => {
      const target = event.target as Node;
      if (!panel.contains(target) && !menu.trigger?.contains(target)) menu.close(false);
    };
    const escape = (event: KeyboardEvent) => {
      if (event.key !== "Escape") return;
      event.preventDefault();
      event.stopImmediatePropagation();
      menu.close();
    };
    window.addEventListener("pointerdown", outside, true);
    window.addEventListener("keydown", escape, true);
    window.addEventListener("resize", place);
    window.addEventListener("scroll", place, true);
    return () => {
      window.removeEventListener("pointerdown", outside, true);
      window.removeEventListener("keydown", escape, true);
      window.removeEventListener("resize", place);
      window.removeEventListener("scroll", place, true);
      menu.content = undefined;
    };
  });
  let search = "";
  let searchedAt = 0;
  return <div {...others} ref={node => { panel = node; }} role="menu" tabindex={-1}
    aria-label={menu.trigger?.getAttribute("aria-label") ?? "Actions"}
    data-slot="dropdown-menu-content"
    class={cn("fixed z-50 max-h-[calc(100dvh-1rem)] min-w-[9rem] max-w-[calc(100vw-1rem)] overflow-y-auto rounded-md border border-border bg-popover p-1 text-popover-foreground shadow-md outline-none", local.class)}
    style={position()}
    onKeyDown={(event) => {
      const items = menuItems(panel);
      const index = items.indexOf(document.activeElement as HTMLButtonElement);
      let next: number | undefined;
      if (event.key === "ArrowDown") next = (index + 1) % items.length;
      if (event.key === "ArrowUp") next = (index - 1 + items.length) % items.length;
      if (event.key === "Home") next = 0;
      if (event.key === "End") next = items.length - 1;
      if (event.key === "Tab") { menu.close(false); return; }
      if (next !== undefined) { event.preventDefault(); items[next]?.focus(); return; }
      if (event.key.length === 1 && !event.ctrlKey && !event.metaKey && !event.altKey && event.key !== " ") {
        const now = Date.now();
        search = (now - searchedAt > 700 ? "" : search) + event.key.toLowerCase();
        searchedAt = now;
        const nextItem = [...items.slice(index + 1), ...items.slice(0, index + 1)]
          .find(item => item.textContent?.trim().toLowerCase().startsWith(search));
        if (nextItem) { event.preventDefault(); nextItem.focus(); }
      }
    }} />;
}
type DropdownMenuItemProps = Omit<ComponentProps<"button">, "onSelect"> & {
  variant?: "default" | "destructive";
  onSelect?: () => void;
};
function DropdownMenuItem(props: DropdownMenuItemProps) {
  const menu = menuContext();
  const local = props;
  const others = omit(props, "class", "variant", "onSelect", "role");
  return <button type="button" {...others} role={local.role ?? "menuitem"} tabindex={-1}
    class={cn("relative flex w-full cursor-default select-none items-center gap-2 rounded-sm px-2 py-1.5 text-left text-sm outline-none transition-colors hover:bg-accent hover:text-accent-foreground focus:bg-accent focus:text-accent-foreground disabled:pointer-events-none disabled:opacity-50 [&_svg]:size-4 [&_svg]:shrink-0", local.variant === "destructive" && "text-destructive hover:bg-destructive/10 focus:bg-destructive/10 focus:text-destructive", local.class)}
    data-slot="dropdown-menu-item"
    onClick={() => { menu.close(); local.onSelect?.(); }} />;
}
function DropdownMenuSeparator(props: ComponentProps<"div">) {
  const local = props;
  const others = omit(props, "class");
  return <div {...others} role="separator" class={cn("-mx-1 my-1 h-px bg-border", local.class)} data-slot="dropdown-menu-separator" />;
}
export { DropdownMenu, DropdownMenuTrigger, DropdownMenuContent, DropdownMenuItem, DropdownMenuSeparator };
