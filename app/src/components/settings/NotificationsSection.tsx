// Settings → Notifications: the tab title badge preference and the browser
// desktop-notification permission.
//
// Permission is requested ONLY from the quiet row below (never on load, per the
// "no notification slot machine" rule). App.tsx fires a silent notification for
// a NEW blocking judgment while the tab is hidden, but only once this reads
// "granted".
import { createSignal, type JSX, Show } from "solid-js";
import { setTitleBadgeEnabled, titleBadgeEnabled } from "../../lib/prefs";
import { Button } from "../ui/button";
import { Group, Field, SectionHeader, Toggle } from "./rows";

export function NotificationsSection(): JSX.Element {
  const notificationsSupported = typeof Notification !== "undefined";
  const [notifPermission, setNotifPermission] = createSignal<NotificationPermission>(
    notificationsSupported ? Notification.permission : "denied",
  );
  async function requestNotifications() {
    if (!notificationsSupported) return;
    try {
      setNotifPermission(await Notification.requestPermission());
    } catch {
      /* best-effort; a browser that rejects the request just stays off */
    }
  }

  return (
    <>
      <SectionHeader>Notifications</SectionHeader>
      <Group class="divide-y divide-border">
        <div class="flex items-center gap-3 py-3">
          <Field
            title="Tab title badge"
            subtitle="Open judgments that need you show as “(3) hirsel” in this browser tab."
          />
          <Toggle
            ariaLabel="Tab title badge"
            checked={titleBadgeEnabled()}
            onChange={setTitleBadgeEnabled}
          />
        </div>
        <div class="flex items-center gap-3 py-3">
          <Field
            title="Desktop notifications"
            subtitle={
              !notificationsSupported
                ? "Not supported in this browser."
                : notifPermission() === "granted"
                  ? "On — a new blocking judgment notifies you while this tab is in the background."
                  : notifPermission() === "denied"
                    ? "Blocked. Re-allow notifications for this site in your browser to turn them on."
                    : "Off — get one quiet notification for a new blocking judgment while the tab is backgrounded."
            }
          />
          <Show
            when={notificationsSupported && notifPermission() === "default"}
            fallback={
              <span
                class="shrink-0 text-xs font-medium"
                classList={{
                  "text-status-success": notifPermission() === "granted",
                  "text-muted-foreground": notifPermission() !== "granted",
                }}
              >
                {notifPermission() === "granted" ? "Enabled" : "Off"}
              </span>
            }
          >
            <Button size="sm" variant="outline" class="shrink-0" onClick={requestNotifications}>
              Enable
            </Button>
          </Show>
        </div>
      </Group>
    </>
  );
}
