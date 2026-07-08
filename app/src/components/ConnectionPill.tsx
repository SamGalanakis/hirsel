import { useStore } from "../store/store";
import styles from "./ConnectionPill.module.css";

const LABEL: Record<string, string> = {
  connecting: "connecting…",
  connected: "connected",
  reconnecting: "reconnecting…",
};

export function ConnectionPill() {
  const connection = useStore((s) => s.connection);
  return (
    <span className={`${styles.pill} ${styles[connection]}`}>
      <span className={styles.dot} aria-hidden />
      {LABEL[connection]}
    </span>
  );
}
