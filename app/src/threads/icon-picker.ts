import { createSignal } from "solid-js";
import { historyId } from "../lib/history";
import type { Thread } from "./types";

export const [threadIconTarget, setThreadIconTarget] = createSignal<{ thread: Thread; history: string } | null>(null);
export function openThreadIconPicker(thread: Thread): void {
  // Freeze the displayed revision: a concurrent edit must be rejected by the Host.
  const history = historyId();
  if (history) setThreadIconTarget({ thread: { ...thread }, history });
}
export function threadIconError(value: string | null): string | null {
  if (value === null) return null;
  if (!value.trim()) return "Choose an emoji or symbol, or use the default icon.";
  if (/\p{Cc}|\u2028|\u2029/u.test(value)) return "Use an emoji or symbol without line breaks or control characters.";
  if (Array.from(value).length > 16 || new TextEncoder().encode(value).length > 64) return "Keep the icon to 16 characters or fewer.";
  return null;
}
export const threadIconPresets = [
  { icon: "🌱", label: "Seedling" }, { icon: "🛠️", label: "Tools" },
  { icon: "📚", label: "Books" }, { icon: "💡", label: "Idea" },
  { icon: "🚀", label: "Rocket" }, { icon: "🎨", label: "Art" },
  { icon: "🏡", label: "Home" }, { icon: "🧭", label: "Compass" },
  { icon: "🛒", label: "Shopping" }, { icon: "🌍", label: "Globe" },
  { icon: "🎯", label: "Target" }, { icon: "⭐", label: "Star" },
];
