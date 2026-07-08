import { useState } from "react";
import type { FormEvent } from "react";
import styles from "./TokenGate.module.css";

interface Props {
  onSubmit: (token: string) => void;
}

/** First-run prompt for the bearer token (protocol.md Auth). Persisted to
 * localStorage by the caller once submitted; this component only collects it. */
export function TokenGate({ onSubmit }: Props) {
  const [value, setValue] = useState("");

  function handleSubmit(e: FormEvent) {
    e.preventDefault();
    const trimmed = value.trim();
    if (trimmed.length === 0) return;
    onSubmit(trimmed);
  }

  return (
    <div className={styles.wrap}>
      <h1 className={styles.title}>hirsel</h1>
      <p className={styles.subtitle}>
        Enter the access token for your Hirsel Host. It's stored only on this
        device.
      </p>
      <form className={styles.form} onSubmit={handleSubmit}>
        <input
          className={styles.input}
          type="password"
          inputMode="text"
          autoComplete="off"
          autoCapitalize="off"
          autoCorrect="off"
          spellCheck={false}
          placeholder="access token"
          value={value}
          onChange={(e) => setValue(e.target.value)}
          autoFocus
        />
        <button className={styles.submit} type="submit" disabled={value.trim().length === 0}>
          Connect
        </button>
      </form>
    </div>
  );
}
