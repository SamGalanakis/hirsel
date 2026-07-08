import { useStore } from "../../store/store";
import styles from "./AgentActivityIndicator.module.css";

/** Subtle "Agent is working…" indicator driven by agent_activity events.
 * Ephemeral by design: never persisted, never replayed (protocol.md). */
export function AgentActivityIndicator() {
  const activity = useStore((s) => s.agentActivity);

  if (activity.state !== "thinking") return null;

  return (
    <div className={styles.wrap}>
      <span className={styles.spinner} aria-hidden />
      <span className={styles.text}>{activity.text ?? "Agent is working…"}</span>
    </div>
  );
}
