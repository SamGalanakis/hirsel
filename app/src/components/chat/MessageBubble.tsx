import { Clock, Copy, GitFork, ListTree, MoreHorizontal, Reply, RotateCcw, X } from "lucide-solid";
import { createSignal, Show } from "solid-js";
import type { DisplayMessage, TimelineEvent } from "../../store/types";
import { copyWithToast } from "../../lib/clipboard";
import { Markdown } from "../Markdown";
import { Bubble, BubbleContent } from "../ui/bubble";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "../ui/dropdown-menu";
import { Message, MessageContent, MessageFooter } from "../ui/message";
import { MessageAttachments } from "./MessageAttachments";
import { QuotedRef } from "./QuotedRef";
import { TurnDetails } from "./Timeline";
import { CommittedToolCalls } from "./ToolCalls";

interface Props {
  message: DisplayMessage;
  refTarget: DisplayMessage | undefined;
  /** v1.5: the finished turn's timeline, retained in session memory for this
   * committed agent message. When present it supersedes the tool_calls chip. */
  turnDetails?: TimelineEvent[];
  /** v2.0 (ADR-0008): this owner message is a Side Chat conclusion landing in
   * main — client-derived (the wire carries no marker; see reducer.ts). Renders
   * a small non-interactive provenance chip so it never reads as amnesia later.
   * Never a link — the side transcript is discarded on conclude, so there is
   * nothing to view (critique P2). */
  isConclusion?: boolean;
  /** Whether to draw the quoted-reply preview. False for the ordinary adjacent
   * back-and-forth (the ref is the immediately-preceding message); true only
   * when the ref is non-contiguous and the citation actually aids orientation. */
  showQuote: boolean;
  highlighted: boolean;
  queued: boolean;
  /** v-transcript: false for a message clustered under a same-author neighbour
   * within a few minutes — suppresses the per-bubble timestamp/meta so a run of
   * messages carries a single footer at the cluster boundary. Defaults to true
   * (every bubble shows its own meta) so other importers are unaffected. */
  showMeta?: boolean;
  /** v-transcript: "Reply" from the message actions menu quotes this message
   * into the composer. Omitted by callers with no reply target (the menu item
   * is then hidden), keeping this additive for the side-chat importer. */
  onReply?: (id: number) => void;
  onTapQuote: (id: number) => void;
  onOpenImage: (src: string, alt: string) => void;
  onRetry: (clientId: string) => void;
  onCancelQueued: (clientId: string) => void;
}

function formatTime(ts: string): string {
  try {
    return new Date(ts).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
  } catch {
    return "";
  }
}

export function MessageBubble(props: Props) {
  const isOwner = () => props.message.author === "owner";
  const showMeta = () => props.showMeta ?? true;
  const [menuOpen, setMenuOpen] = createSignal(false);
  const [turnOpen, setTurnOpen] = createSignal(false);
  const hasTurnDetails = () => !!props.turnDetails && props.turnDetails.length > 0;

  function copyBody() {
    void copyWithToast(props.message.body, "Copied message");
  }

  function reply() {
    props.onReply?.(props.message.id);
  }

  function viewTurn() {
    setTurnOpen(true);
    // Let the panel mount, then bring it into view.
    queueMicrotask(() =>
      document
        .getElementById(`msg-${props.message.id}`)
        ?.querySelector('[data-slot="turn-details"]')
        ?.scrollIntoView({ behavior: "smooth", block: "nearest" }),
    );
  }

  const canReply = () => !!props.onReply && props.message.id >= 0;

  return (
    <Message
      id={`msg-${props.message.id}`}
      align={isOwner() ? "end" : "start"}
      class="scroll-mt-4 px-3"
    >
      <MessageContent>
        <Bubble
          variant={isOwner() ? "default" : "ghost"}
          align={isOwner() ? "end" : "start"}
        >
          <BubbleContent
            class="transition-shadow"
            classList={{
              // Owner chip: a ring hugs the rounded fill. Agent prose has no
              // fill/padding, so its highlight is a quiet inset wash whose
              // negative margins cancel the added padding (no layout shift).
              "ring-2 ring-ring": props.highlighted && isOwner(),
              "-mx-2 -my-1 rounded-md bg-primary/5 px-2 py-1 ring-1 ring-ring/70":
                props.highlighted && !isOwner(),
            }}
            onContextMenu={(e) => {
              e.preventDefault();
              setMenuOpen(true);
            }}
          >
            <Show when={props.showQuote}>
              <QuotedRef
                message={props.refTarget}
                onTap={() => props.onTapQuote(props.message.ref as number)}
              />
            </Show>
            <Show when={props.message.body.length > 0}>
              <Markdown>{props.message.body}</Markdown>
            </Show>
            <Show when={props.message.attachments && props.message.attachments.length > 0}>
              <div classList={{ "mt-2": props.message.body.length > 0 }}>
                <MessageAttachments
                  attachments={props.message.attachments}
                  onOpenImage={props.onOpenImage}
                />
              </div>
            </Show>
          </BubbleContent>
        </Bubble>
        <MessageFooter class="gap-2 pt-0.5 text-[0.68rem]">
          {/* Queued (next_turn while a turn is active): cancellable chip. */}
          <Show when={props.queued && props.message.clientId}>
            <span class="inline-flex items-center gap-1 rounded-full bg-status-attention/15 px-1.5 py-px text-status-attention">
              <Clock class="size-3" aria-hidden="true" />
              queued
              <button
                type="button"
                class="ml-0.5 -mr-0.5 rounded-full p-px transition-colors hover:bg-status-attention/25"
                aria-label="Cancel queued message"
                onClick={() => props.onCancelQueued(props.message.clientId as string)}
              >
                <X class="size-3" />
              </button>
            </span>
          </Show>
          {/* Failed send: retry affordance. */}
          <Show when={props.message.failed && props.message.clientId}>
            <button
              type="button"
              class="inline-flex items-center gap-1 rounded-full bg-destructive/15 px-1.5 py-px text-destructive transition-colors hover:bg-destructive/25"
              onClick={() => props.onRetry(props.message.clientId as string)}
            >
              <RotateCcw class="size-3" aria-hidden="true" />
              Failed · retry
            </button>
          </Show>
          {/* Plain in-flight send. */}
          <Show when={props.message.pending && !props.message.failed && !props.queued}>
            <span class="italic">sending…</span>
          </Show>
          {/* v2.0 provenance chip + timestamp: the calm "meta" that collapses to
              one per cluster (only rendered at a cluster boundary). */}
          <Show when={showMeta()}>
            <Show when={props.isConclusion}>
              <span class="inline-flex items-center gap-1 rounded-full bg-muted px-1.5 py-px text-muted-foreground">
                <GitFork class="size-3" aria-hidden="true" />
                worked out in a side chat
              </span>
            </Show>
            <span>{formatTime(props.message.ts)}</span>
          </Show>
          {/* Per-message actions. On a fine pointer the ⋯ is hover-revealed
              (hidden at rest, appears on message hover or keyboard focus) so it
              is not standing meta-row noise; right-clicking the bubble opens the
              same menu. On touch (no hover) it stays quietly visible and is also
              reachable by long-press (the bubble context menu). */}
          <DropdownMenu open={menuOpen()} onOpenChange={setMenuOpen}>
            <DropdownMenuTrigger
              class="rounded p-0.5 text-muted-foreground opacity-0 transition-opacity hover:text-foreground focus-visible:opacity-100 group-hover/message:opacity-100 data-[expanded]:opacity-100 pointer-coarse:opacity-60"
              aria-label="Message actions"
            >
              <MoreHorizontal class="size-3.5" />
            </DropdownMenuTrigger>
            <DropdownMenuContent class="min-w-[10rem]">
              <Show when={canReply()}>
                <DropdownMenuItem onSelect={reply}>
                  <Reply class="text-muted-foreground" />
                  Reply
                </DropdownMenuItem>
              </Show>
              <DropdownMenuItem onSelect={copyBody}>
                <Copy class="text-muted-foreground" />
                Copy
              </DropdownMenuItem>
              <Show when={hasTurnDetails()}>
                <DropdownMenuSeparator />
                <DropdownMenuItem onSelect={viewTurn}>
                  <ListTree class="text-muted-foreground" />
                  View turn details
                </DropdownMenuItem>
              </Show>
            </DropdownMenuContent>
          </DropdownMenu>
        </MessageFooter>
        {/* Finished-turn affordance under the (agent) bubble. A captured v1.5
            timeline wins (full "turn details" panel); otherwise the v1.4
            "⚙ N tools" chip is the fallback for replayed messages. */}
        <Show
          when={hasTurnDetails()}
          fallback={
            <Show when={props.message.tool_calls && props.message.tool_calls.length > 0}>
              <div class="pt-1">
                <CommittedToolCalls toolCalls={props.message.tool_calls ?? []} />
              </div>
            </Show>
          }
        >
          <div class="pt-1">
            <TurnDetails
              events={props.turnDetails ?? []}
              expanded={turnOpen()}
              onExpandedChange={setTurnOpen}
            />
          </div>
        </Show>
      </MessageContent>
    </Message>
  );
}
