/** A durable Thread identity cue, independent of activity or lifecycle state. */
const tones = [
  "bg-emerald-100 text-emerald-900 dark:bg-emerald-950 dark:text-emerald-200",
  "bg-amber-100 text-amber-900 dark:bg-amber-950 dark:text-amber-200",
  "bg-sky-100 text-sky-900 dark:bg-sky-950 dark:text-sky-200",
  "bg-rose-100 text-rose-900 dark:bg-rose-950 dark:text-rose-200",
  "bg-violet-100 text-violet-900 dark:bg-violet-950 dark:text-violet-200",
];
export interface ThreadAvatarIdentity { id: number; kind: "space" | "task"; title: string; icon?: string | null }
export function ThreadAvatar(props: { thread: ThreadAvatarIdentity; small?: boolean }) {
  return <span aria-hidden="true" data-slot="thread-avatar" data-thread-avatar={props.thread.id}
    data-thread-kind={props.thread.kind} class={`inline-flex shrink-0 select-none items-center justify-center overflow-hidden align-middle font-medium leading-none ${props.thread.kind === "space" ? "rounded-md" : "rounded-full"} ${props.small ? "size-5 text-xs" : "size-7 text-sm"} ${tones[Math.abs(props.thread.id) % tones.length]}`}>
    {props.thread.icon ?? (Array.from(props.thread.title.trim())[0]?.toLocaleUpperCase() || "#")}
  </span>;
}
