import { Inbox, MessageCircle } from "lucide-solid";
import { Show } from "solid-js";
import { openRequiresResponseCount } from "../store/selectors";
import { setActiveTab, state } from "../store/store";

export function TabBar() {
  const badgeCount = () => openRequiresResponseCount(state.inbox);

  return (
    <nav class="flex flex-shrink-0 border-t border-border bg-card pb-[env(safe-area-inset-bottom)]">
      <button
        type="button"
        class="relative flex flex-1 flex-col items-center gap-0.5 py-2 pb-3 text-xs transition-colors"
        classList={{
          "text-primary": state.activeTab === "chat",
          "text-muted-foreground": state.activeTab !== "chat",
        }}
        onClick={() => setActiveTab("chat")}
        aria-current={state.activeTab === "chat"}
      >
        <MessageCircle class="size-5" aria-hidden="true" />
        Chat
      </button>
      <button
        type="button"
        class="relative flex flex-1 flex-col items-center gap-0.5 py-2 pb-3 text-xs transition-colors"
        classList={{
          "text-primary": state.activeTab === "inbox",
          "text-muted-foreground": state.activeTab !== "inbox",
        }}
        onClick={() => setActiveTab("inbox")}
        aria-current={state.activeTab === "inbox"}
      >
        <Inbox class="size-5" aria-hidden="true" />
        Inbox
        <Show when={badgeCount() > 0}>
          <span class="absolute top-1 right-[calc(50%-1.375rem)] grid h-4 min-w-4 place-items-center rounded-full bg-status-danger px-1 text-[0.65rem] font-bold text-primary-foreground">
            {badgeCount() > 99 ? "99+" : badgeCount()}
          </span>
        </Show>
      </button>
    </nav>
  );
}
