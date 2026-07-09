import { Activity, Inbox, MessageCircle } from "lucide-solid";
import type { Component, ComponentProps } from "solid-js";
import { Show } from "solid-js";
import { openUnreadCount, runningProcessCount } from "../store/selectors";
import { setActiveTab, state, type Tab } from "../store/store";

interface TabDef {
  tab: Tab;
  label: string;
  icon: Component<ComponentProps<"svg">>;
  badge: () => number;
  /** Running processes read as "active", not "needs attention", so the badge
   * uses the active accent rather than the Inbox danger accent. */
  badgeTone: "danger" | "active";
}

export function TabBar() {
  const tabs: TabDef[] = [
    { tab: "chat", label: "Chat", icon: MessageCircle, badge: () => 0, badgeTone: "active" },
    {
      tab: "inbox",
      label: "Inbox",
      icon: Inbox,
      badge: () => openUnreadCount(state.inbox, state.unreadOverrides),
      badgeTone: "danger",
    },
    {
      tab: "processes",
      label: "Processes",
      icon: Activity,
      badge: () => runningProcessCount(state.processes),
      badgeTone: "active",
    },
  ];

  return (
    <nav class="flex flex-shrink-0 border-t border-border bg-card pb-[env(safe-area-inset-bottom)]">
      {tabs.map((t) => {
        const active = () => state.activeTab === t.tab;
        return (
          <button
            type="button"
            class="relative flex flex-1 flex-col items-center gap-0.5 py-2 pb-3 text-xs transition-colors"
            classList={{
              "text-primary": active(),
              "text-muted-foreground": !active(),
            }}
            onClick={() => setActiveTab(t.tab)}
            aria-current={active()}
          >
            <t.icon class="size-5" aria-hidden="true" />
            {t.label}
            <Show when={t.badge() > 0}>
              <span
                class="absolute top-1 right-[calc(50%-1.375rem)] grid h-4 min-w-4 place-items-center rounded-full px-1 text-[0.65rem] font-bold text-primary-foreground"
                classList={{
                  "bg-status-danger": t.badgeTone === "danger",
                  "bg-status-active": t.badgeTone === "active",
                }}
              >
                {t.badge() > 99 ? "99+" : t.badge()}
              </span>
            </Show>
          </button>
        );
      })}
    </nav>
  );
}
