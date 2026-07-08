import { useEffect, useState } from "react";
import styles from "./App.module.css";
import { useStore } from "./store/store";
import { useTitleBadge } from "./hooks/useTitleBadge";
import { getStoredToken, setStoredToken, startClient } from "./ws/client";
import { ConnectionPill } from "./components/ConnectionPill";
import { TabBar } from "./components/TabBar";
import { TokenGate } from "./components/TokenGate";
import { ChatView } from "./components/chat/ChatView";
import { InboxView } from "./components/inbox/InboxView";

// VITE_WS_URL always wins. Otherwise: in dev, default to the mock server's
// port; in production the Hirsel Host serves this app from the same origin
// with its WS endpoint at /ws, so default to same-origin.
const WS_URL =
  import.meta.env.VITE_WS_URL ??
  (import.meta.env.DEV
    ? `ws://${window.location.hostname}:8787`
    : `${window.location.protocol === "https:" ? "wss://" : "ws://"}${window.location.host}/ws`);

function App() {
  const [token, setToken] = useState<string | null>(() => getStoredToken());
  const activeTab = useStore((s) => s.activeTab);
  useTitleBadge();

  useEffect(() => {
    if (!token) return;
    const client = startClient(WS_URL, token);
    return () => client.close();
  }, [token]);

  if (!token) {
    return (
      <div className={styles.app}>
        <TokenGate
          onSubmit={(t) => {
            setStoredToken(t);
            setToken(t);
          }}
        />
      </div>
    );
  }

  return (
    <div className={styles.app}>
      <header className={styles.header}>
        <h1 className={styles.title}>hirsel</h1>
        <ConnectionPill />
      </header>
      <main className={styles.main}>{activeTab === "chat" ? <ChatView /> : <InboxView />}</main>
      <TabBar />
    </div>
  );
}

export default App;
