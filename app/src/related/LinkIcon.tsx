import { ArrowUpRight, CircleAlert, GitBranch, MessageCircle } from "../components/ui/icons";
import { Dynamic } from "@solidjs/web";
import type { WebLink } from "./url";
export function LinkIcon(props: { kind: WebLink["kind"] | "thread"; class?: string }) {
  const icon = () => ({ thread: MessageCircle, "pull-request": GitBranch, repository: GitBranch, issue: CircleAlert, web: ArrowUpRight })[props.kind];
  return <Dynamic component={icon()} class={props.class ?? "size-3.5 shrink-0"} aria-hidden="true" />;
}
