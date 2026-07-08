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

const WS_URL = import.meta.env.VITE_WS_URL ?? `ws://${window.location.hostname}:8787`;

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
