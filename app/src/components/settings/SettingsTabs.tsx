// The Settings tab list: a quiet rail down the left of the Settings column at
// `rail:` and up, a horizontal strip above the content below it. One component
// for both, because it is one list — only its axis changes.
//
// Quiet means what DESIGN means (§3, §6): sentence-case labels at body size, no
// boxes, no all-caps. Selection is carried by foreground weight plus one mint
// indicator on the active tab's leading edge — the tab's left edge in the rail,
// its bottom edge in the strip.
import { For, Show } from "solid-js";

import type { SettingsTab } from "../../store/store";

/** Eight flat sections was a list to read, not a place to navigate: nothing
 * said which of them were about the machine, which about the agents, and which
 * about this device, so the Owner had to read all eight labels every time.
 * They are the SAME eight sections, in four named groups. The rail shows the
 * groups as labelled columns; the strip below `rail:` wraps the eight tabs
 * into as many rows as the width needs, every label whole, and drops the
 * group headings — a sideways-scrolling strip clipped its last label and a
 * second caps row said nothing a wrapped row does not. */
export const SETTINGS_GROUPS: readonly { heading: string; tabs: readonly { id: SettingsTab; label: string }[] }[] = [
  { heading: "Look & feel", tabs: [
    { id: "appearance", label: "Appearance" },
    { id: "notifications", label: "Notifications" },
  ] },
  { heading: "Agents", tabs: [
    { id: "agents", label: "Thread models" },
    { id: "providers", label: "Providers" },
    { id: "plugins", label: "Plugins" },
  ] },
  { heading: "This device", tabs: [
    { id: "connection", label: "Connection & devices" },
  ] },
  { heading: "Help", tabs: [
    { id: "guide", label: "Guide" },
    { id: "about", label: "About & debug" },
  ] },
];

/** The flat reading order the roving tabindex walks. */
export const SETTINGS_TABS: readonly { id: SettingsTab; label: string }[] =
  SETTINGS_GROUPS.flatMap((group) => group.tabs);

export const settingsTabId = (tab: SettingsTab) => `settings-tab-${tab}`;
export const settingsPanelId = (tab: SettingsTab) => `settings-panel-${tab}`;

export function SettingsTabs(props: {
  active: SettingsTab;
  onSelect: (tab: SettingsTab) => void;
}) {
  let listRef: HTMLDivElement | undefined;

  /** Activation follows focus: these panels are cheap, so arrowing through the
   * list shows each one rather than making the Owner press Enter to look. */
  function move(index: number) {
    const total = SETTINGS_TABS.length;
    const next = SETTINGS_TABS[((index % total) + total) % total];
    props.onSelect(next.id);
    queueMicrotask(() =>
      listRef?.querySelector<HTMLElement>(`[data-tab="${next.id}"]`)?.focus(),
    );
  }

  // Both axes move the selection: the same list is a vertical rail at `rail:`
  // and a horizontal strip below it, and the keys that reach for it differ with
  // what the Owner is looking at.
  function onKeyDown(event: KeyboardEvent) {
    const index = SETTINGS_TABS.findIndex((tab) => tab.id === props.active);
    switch (event.key) {
      case "ArrowRight":
      case "ArrowDown":
        move(index + 1);
        break;
      case "ArrowLeft":
      case "ArrowUp":
        move(index - 1);
        break;
      case "Home":
        move(0);
        break;
      case "End":
        move(SETTINGS_TABS.length - 1);
        break;
      default:
        return;
    }
    event.preventDefault();
  }

  return (
    <div
      ref={(node) => {
        listRef = node;
      }}
      role="tablist"
      aria-label="Settings sections"
      // The list itself is never a tab stop — the roving tabindex on the tabs
      // is what the keyboard reaches. It stays programmatically focusable so
      // the role is honestly addressable.
      tabindex={-1}
      data-slot="settings-tabs"
      onKeyDown={onKeyDown}
      class="sticky top-0 z-10 flex shrink-0 flex-wrap gap-x-1 gap-y-0.5 bg-background pb-2 rail:top-6 rail:max-h-[calc(100dvh-6rem)] rail:w-52 rail:flex-col rail:flex-nowrap rail:gap-0 rail:overflow-y-auto rail:pb-0"
    >
      <For each={SETTINGS_GROUPS}>
        {(group) => (
          <div data-slot="settings-group" class="contents rail:flex rail:flex-col rail:gap-0.5 rail:pt-4 rail:first:pt-0">
            <p
              aria-hidden="true"
              class="hidden px-3 pb-1 text-meta font-medium uppercase tracking-wide text-muted-foreground rail:block"
            >
              {group.heading}
            </p>
            <div class="contents rail:flex rail:flex-col rail:gap-0">
            <For each={group.tabs}>
              {(tab) => {
                const active = () => props.active === tab.id;
                return (
                  <button
                    type="button"
                    role="tab"
                    id={settingsTabId(tab.id)}
                    data-tab={tab.id}
                    aria-selected={(active()) ? "true" : "false"}
                    aria-controls={settingsPanelId(tab.id)}
                    // Roving tabindex: the whole list is ONE tab stop, and the arrow
                    // keys move within it.
                    tabindex={active() ? 0 : -1}
                    onClick={() => props.onSelect(tab.id)}
                    class={["relative shrink-0 whitespace-nowrap rounded-md px-2 py-1.5 text-left text-sm outline-none transition-colors focus-visible:ring-2 focus-visible:ring-ring [@media(pointer:coarse)]:min-h-11 rail:px-3", {
                      "font-medium text-foreground": active(),
                      "text-muted-foreground hover:text-foreground": !active(),
                    }]}
                  >
                    {tab.label}
                    <Show when={active()}>
                      <span
                        aria-hidden="true"
                        class="absolute inset-x-2 bottom-0 h-0.5 rounded-full bg-primary rail:inset-x-auto rail:bottom-1.5 rail:left-0 rail:top-1.5 rail:h-auto rail:w-0.5"
                      />
                    </Show>
                  </button>
                );
              }}
            </For>
            </div>
          </div>
        )}
      </For>
    </div>
  );
}
