/** The Thread icon vocabulary, mirroring crates/hirsel-proto/src/thread_icon.rs.
 * thread-symbols.test.ts fails if the two drift. */
export const THREAD_SYMBOL_GROUPS = [
  { label: "Work", names: ["hammer", "wrench", "bug", "flask", "rocket", "package", "git-branch", "terminal"] },
  { label: "Knowledge", names: ["book", "file-text", "lightbulb", "graduation-cap", "brain", "search"] },
  { label: "People & places", names: ["users", "home", "building", "globe", "map-pin"] },
  { label: "Money & time", names: ["wallet", "receipt", "calendar", "clock", "timer"] },
  { label: "Comms", names: ["mail", "message-square", "bell", "megaphone"] },
  { label: "Media", names: ["image", "music", "film", "camera"] },
  { label: "Other", names: ["star", "heart", "flag", "tag", "shield", "key", "zap", "leaf", "sun", "moon", "coffee", "gift", "puzzle"] },
] as const satisfies readonly { label: string; names: readonly string[] }[];

export type ThreadSymbol = (typeof THREAD_SYMBOL_GROUPS)[number]["names"][number];

export const THREAD_SYMBOLS: readonly ThreadSymbol[] = THREAD_SYMBOL_GROUPS.flatMap(group => [...group.names]);

export const THREAD_TINTS = ["neutral", "red", "orange", "amber", "green", "teal", "blue", "violet", "pink"] as const;
export type ThreadTint = (typeof THREAD_TINTS)[number];

export function isThreadSymbol(name: string): name is ThreadSymbol {
  return (THREAD_SYMBOLS as readonly string[]).includes(name);
}

/** Search over the vocabulary: hyphens are word breaks, so "git b" finds git-branch. */
export function matchesThreadSymbol(name: ThreadSymbol, query: string): boolean {
  const needle = query.trim().toLowerCase();
  if (!needle) return true;
  const words = name.split("-");
  return name.includes(needle) || words.some(word => word.startsWith(needle))
    || needle.split(/[\s-]+/).every(part => words.some(word => word.startsWith(part)));
}

/** The tile colours, as CSS custom properties defined in styles.css. */
export function tintStyle(tint: ThreadTint): { "background-color": string; color: string } {
  return { "background-color": `var(--tint-${tint})`, color: `var(--tint-${tint}-foreground)` };
}

/** The default mark: initials of the first two words, or the first letter. */
export function threadMonogram(title: string): string {
  const words = title.trim().split(/\s+/).filter(Boolean);
  if (words.length === 0) return "#";
  const letters = (words.length > 1 ? words.slice(0, 2) : words.slice(0, 1))
    .map(word => Array.from(word)[0] ?? "")
    .join("");
  return letters.toLocaleUpperCase();
}
