import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import styles from "./Markdown.module.css";

interface Props {
  children: string;
}

/** Shared markdown renderer for Chat messages and Inbox Item content.
 * react-markdown does not render raw HTML by default, so this is safe
 * against agent-authored markdown without an extra sanitizer dependency. */
export function Markdown({ children }: Props) {
  return (
    <div className={styles.md} data-testid="markdown">
      <ReactMarkdown remarkPlugins={[remarkGfm]}>{children}</ReactMarkdown>
    </div>
  );
}
