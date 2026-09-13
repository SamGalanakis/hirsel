import { createSignal, For, Show } from "solid-js";
import { ChevronDown } from "../components/ui/icons";
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger } from "../components/ui/dropdown-menu";
import { useRelatedOrigin } from "../related/context";
import { openersFor, type ArtifactOpenerId } from "./render-mode";
import { previewArtifact } from "./openers";
import { captureShowcaseOrigin, setThreadShowcase, type ShowcaseOrigin } from "./showcase-actions";
import { artifactState, downloadArtifactById, openerError } from "./store";

const button = "inline-flex min-h-11 min-w-11 items-center justify-center gap-1 rounded-lg px-2 text-sm text-muted-foreground hover:bg-muted hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring";

/** The one dispatcher: every way of opening a result, derived from its kind.
 * The card's plain click runs the first opener; this menu names the rest. */
export function ArtifactOpenWith(props: { artifactId: number }) {
  const origin = useRelatedOrigin();
  const artifact = () => artifactState.summaries.find(row => row.id === props.artifactId);
  let target: ShowcaseOrigin | null = null;
  const [error, setError] = createSignal<string | null>(null);
  const failure = () => error() ?? (openerError()?.id === props.artifactId ? openerError()!.message : null);
  const openers = () => {
    const summary = artifact();
    if (!summary) return [];
    // Showcasing needs a real addressed Thread; the rest never do.
    return openersFor(summary).filter(opener => opener.id !== "showcase" || origin !== null);
  };
  const activate = (id: ArtifactOpenerId) => {
    const summary = artifact();
    const title = summary?.title ?? `Artifact #${props.artifactId}`;
    setError(null);
    if (id === "preview" || id === "source") { previewArtifact(props.artifactId, title, id === "source" ? "source" : "rendered"); return; }
    if (id === "download") { downloadArtifactById(props.artifactId); return; }
    try {
      if (!target) throw new Error("Reconnect and reopen this menu to choose a showcase.");
      setThreadShowcase(target, props.artifactId);
    } catch (cause) { setError(cause instanceof Error ? cause.message : "Couldn’t showcase this artifact."); }
  };
  return <Show when={openers().length > 0}><div class="shrink-0">
    <DropdownMenu onOpenChange={open => { if (open) { target = captureShowcaseOrigin(origin); setError(null); } }}>
      <DropdownMenuTrigger class={button} aria-label="Open with" title="Open with"><span class="hidden text-xs 2xl:inline">Open with</span><ChevronDown class="size-4" /></DropdownMenuTrigger>
      <DropdownMenuContent><For each={openers()}>{opener => <DropdownMenuItem data-opener={opener.id} onSelect={() => activate(opener.id)}>{opener.label}</DropdownMenuItem>}</For></DropdownMenuContent>
    </DropdownMenu>
    <Show when={failure()}><p role="alert" class="max-w-60 px-2 text-xs text-status-danger">{failure()}</p></Show>
  </div></Show>;
}
