import { useStore } from "../store/store";
import { openRequiresResponseCount } from "../store/selectors";
import styles from "./TabBar.module.css";

export function TabBar() {
  const activeTab = useStore((s) => s.activeTab);
  const setActiveTab = useStore((s) => s.setActiveTab);
  const inboxBadgeCount = useStore((s) => openRequiresResponseCount(s.inbox));

  return (
    <nav className={styles.bar}>
      <button
        type="button"
        className={`${styles.tab} ${activeTab === "chat" ? styles.active : ""}`}
        onClick={() => setActiveTab("chat")}
        aria-current={activeTab === "chat"}
      >
        <span className={styles.icon} aria-hidden>
          💬
        </span>
        Chat
      </button>
      <button
        type="button"
        className={`${styles.tab} ${activeTab === "inbox" ? styles.active : ""}`}
        onClick={() => setActiveTab("inbox")}
        aria-current={activeTab === "inbox"}
      >
        <span className={styles.icon} aria-hidden>
          🗂️
        </span>
        Inbox
        {inboxBadgeCount > 0 && (
          <span className={styles.badge}>{inboxBadgeCount > 99 ? "99+" : inboxBadgeCount}</span>
        )}
      </button>
    </nav>
  );
}
