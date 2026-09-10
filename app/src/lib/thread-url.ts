import type { RelatedTarget } from "../threads/types";
export type ThreadTarget = Extract<RelatedTarget, {kind:"thread"}>;
export type ThreadLinkResult = { kind: "thread"; target: ThreadTarget } | { kind: "incomplete" | "invalid" };
const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
/** Only this app's origin (or a root-relative URL) can claim a local Thread. */
export function parseThreadLink(input: string, origin = location.origin): ThreadLinkResult | null {
  if (input.startsWith("//")) return null;
  if (!input.startsWith("/") && !/^https?:\/\//i.test(input)) return null;
  let url: URL;
  try { url = new URL(input, origin); } catch { return null; }
  if (url.origin !== origin || !url.pathname.startsWith("/t/")) return null;
  if (url.username || url.password || input.includes("\\") || /\s/u.test(input)) return { kind: "invalid" };
  const match = /^\/t\/(0|[1-9]\d*)$/.exec(url.pathname);
  if (!match || !Number.isSafeInteger(Number(match[1]))) return { kind: "invalid" };
  const histories = url.searchParams.getAll("history");
  if (histories.length === 0) return { kind: "incomplete" };
  if (histories.length !== 1 || !UUID.test(histories[0])) return { kind: "invalid" };
  return { kind: "thread", target: { kind: "thread", history_id: histories[0].toLowerCase(), thread_id: Number(match[1]) } };
}
export function threadPath(target: ThreadTarget): string {
  return `/t/${target.thread_id}?history=${encodeURIComponent(target.history_id.toLowerCase())}`;
}
export function threadUrl(target: ThreadTarget, origin = location.origin): string { return new URL(threadPath(target), origin).href; }
export function threadReference(target: ThreadTarget): string { return `[Thread #${target.thread_id}](${threadUrl(target)})`; }
export function plainPrimaryClick(event: MouseEvent): boolean { return event.button === 0 && !event.metaKey && !event.ctrlKey && !event.shiftKey && !event.altKey && !event.defaultPrevented; }
