import { ArrowUp, AtSign, ChevronDown, FileText, LoaderCircle, Paperclip, RotateCcw, Square, X } from "lucide-solid";
import { createEffect, createSignal, For, Show } from "solid-js";
import type { Blob, SendMode } from "../../protocol";
import { state } from "../../store/store";
import type { DisplayMessage } from "../../store/types";
import { anyOverlayOpen } from "../../lib/focus";
import { formatBytes, snippet } from "../../lib/format";
import { handleSubmitKeys } from "../../lib/submitKeymap";
import { toast } from "../../lib/toast";
import { Button } from "../ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "../ui/dropdown-menu";
import { Textarea } from "../ui/textarea";
import { resolveMentionIds } from "./mentions";
import { useMentionPicker } from "./useMentionPicker";
import { useTextInput } from "./useTextInput";
import {
  Attachment,
  AttachmentAction,
  AttachmentActions,
  AttachmentContent,
  AttachmentDescription,
  AttachmentGroup,
  AttachmentMedia,
  AttachmentTitle,
  type AttachmentState,
} from "../ui/attachment";
import type { AttachmentsController } from "./useAttachments";

const MAX_HEIGHT_PX = 112;
const LONG_PRESS_MS = 450;
/** Reply-quote preview length — kept at the composer's tighter 60 chars. */
const REPLY_SNIPPET_MAX = 60;

interface Props {
  replyingTo: DisplayMessage | undefined | null;
  onCancelReply: () => void;
  attachments: AttachmentsController;
  thinking: boolean;
  /** One-shot composer pre-fill (v1.4 "Ask to stop"); consumed once then cleared. */
  prefill?: string | null;
  onConsumePrefill?: () => void;
  onSend: (
    body: string,
    ref: number | null,
    mode: SendMode,
    blobs: Blob[],
    mentions: number[],
  ) => void;
  onStop: () => void;
  getLastOwnerBody: () => string | null;
}

/** Composer anchored at the bottom of Chat, below the Tray shelf. CLI-grade keyboard map on fine-pointer
 * devices (Enter send · Shift+Enter newline · Tab queue next-turn · Esc cancel
 * turn · ArrowUp recall); phone keeps Enter as newline and uses the send button
 * (long-press = queue). Handles attachment staging (paperclip + paste). */
export function Composer(props: Props) {
  // Shared input mechanics (value signal, coarse-pointer detection, auto-grow)
  // with the Inbox inline ReplyInput.
  const { value, setValue, coarse, setRef, focus, caretToEnd } = useTextInput(MAX_HEIGHT_PX, "main");
  const [sending, setSending] = createSignal(false);
  const [focused, setFocused] = createSignal(false);
  const offline = () => state.connection !== "connected";
  let fileInputRef: HTMLInputElement | undefined;
  let textareaRef: HTMLTextAreaElement | undefined;
  let longPressTimer: ReturnType<typeof setTimeout> | undefined;
  let longPressed = false;

  // @-mention picker (v2.1): typing `@` opens a quick-select of open Pings that
  // inserts an `@handle` token; the outgoing `mentions` are re-parsed from the
  // body on send (resolveMentionIds), so text and mentions stay in sync.
  const mentions = useMentionPicker({
    getEl: () => textareaRef,
    value,
    setValue,
  });

  // Focus the composer when a reply is pre-quoted into it.
  createEffect(() => {
    if (props.replyingTo) focus();
  });

  // Consume a one-shot pre-fill (v1.4 "Ask to stop"): drop the text into the
  // draft, move the caret to the end, focus, then clear so it fires once.
  createEffect(() => {
    const pre = props.prefill;
    if (!pre) return;
    setValue(pre);
    focus();
    caretToEnd();
    props.onConsumePrefill?.();
  });

  const uploadState = (clientId: string): AttachmentState => {
    const u = state.uploads.find((x) => x.clientId === clientId);
    if (!u) return "idle";
    return u.state; // "uploading" | "done" | "error"
  };

  async function submit(mode: SendMode) {
    const body = value().trim();
    const hasFiles = props.attachments.files().length > 0;
    if (body.length === 0 && !hasFiles) return;

    let blobs: Blob[] = [];
    if (hasFiles) {
      setSending(true);
      try {
        blobs = await props.attachments.uploadAll();
      } catch {
        setSending(false);
        toast("Some attachments failed — retry them", { variant: "error" });
        return;
      }
      setSending(false);
    }

    // Re-parse the composed text into open-Ping ids for send_message.mentions.
    const mentionIds = resolveMentionIds(body, state.pings);
    props.onSend(body, props.replyingTo?.id ?? null, mode, blobs, mentionIds);
    props.attachments.clear();
    setValue("");
    mentions.close();
    if (props.replyingTo) props.onCancelReply();
    focus();
  }

  function handleKeyDown(e: KeyboardEvent) {
    // The mention picker owns Up/Down/Enter/Tab/Esc ONLY while it is open, so
    // the composer keymap below (Enter=send, Tab=queue, Esc=cancel) is intact
    // whenever the picker is closed.
    if (mentions.handleKeyDown(e)) return;
    // Esc priority is picker → modal/overlay → stop turn. The picker (above)
    // owns Esc while open; an open overlay owns it next — gate stop-on-Esc on
    // `!anyOverlayOpen()` so one Esc dismissing a sheet/dialog never *also*
    // kills a live agent turn behind it; only with nothing else up does Esc
    // interrupt the turn (no-op if idle).
    if (e.key === "Escape") {
      if (props.thinking && !anyOverlayOpen()) {
        e.preventDefault();
        props.onStop();
      }
      return;
    }
    // Shared submit keymap (Cmd/Ctrl+Enter send · coarse guard · Enter send ·
    // ArrowUp recall). Returns true when it consumed the key.
    if (
      handleSubmitKeys(e, {
        value,
        coarse,
        onSend: () => void submit("send"),
        recallLast: props.getLastOwnerBody,
        onRecall: (text) => {
          setValue(text);
          caretToEnd();
        },
      })
    ) {
      return;
    }
    // Tab with a non-empty composer queues a next-turn message (desktop only);
    // empty Tab keeps normal focus movement.
    if (!coarse() && e.key === "Tab" && !e.shiftKey && value().trim().length > 0) {
      e.preventDefault();
      void submit("next_turn");
    }
  }

  function handlePaste(e: ClipboardEvent) {
    const items = e.clipboardData?.items;
    if (!items) return;
    const files: File[] = [];
    for (const item of items) {
      if (item.kind === "file") {
        const f = item.getAsFile();
        if (f) files.push(f);
      }
    }
    if (files.length > 0) {
      e.preventDefault();
      props.attachments.addFiles(files);
    }
  }

  function onSendPointerDown() {
    longPressed = false;
    if (!coarse()) return;
    longPressTimer = setTimeout(() => {
      longPressed = true;
      void submit("next_turn");
    }, LONG_PRESS_MS);
  }
  function onSendPointerUp() {
    if (longPressTimer) clearTimeout(longPressTimer);
  }
  function onSendClick() {
    if (longPressed) {
      longPressed = false;
      return; // the long-press already queued it
    }
    void submit("send");
  }

  const canSend = () => value().trim().length > 0 || props.attachments.files().length > 0;

  return (
    // The bar (border-t + bg-card) bleeds the full center-pane width on desktop;
    // the inner wrapper re-centers the actual composer content at the prose
    // measure (`rail:mx-auto rail:max-w-[680px]`), so the input aligns to the
    // transcript while the bar spans rail-hairline → context-hairline. The
    // measure is `rail:`-gated, so phone/split are pixel-identical to before.
    <div class="flex-shrink-0 border-t border-border bg-card px-3 py-2">
      <div class="w-full rail:mx-auto rail:max-w-[680px]">
      <Show when={props.replyingTo}>
        {(replyingTo) => (
          <div class="mb-2 flex items-start gap-2 rounded-md border-l-2 border-primary bg-muted px-2 py-1">
            <div class="min-w-0 flex-1">
              <div class="text-[0.68rem] uppercase tracking-[0.03em] text-primary">
                Replying to {replyingTo().author === "owner" ? "you" : "Agent"}
              </div>
              <div class="overflow-hidden text-ellipsis whitespace-nowrap text-xs text-muted-foreground">
                {snippet(replyingTo().body, REPLY_SNIPPET_MAX)}
              </div>
            </div>
            <button
              type="button"
              class="p-0.5 text-muted-foreground transition-colors hover:text-foreground"
              onClick={() => props.onCancelReply()}
              aria-label="Cancel reply"
            >
              <X class="size-4" />
            </button>
          </div>
        )}
      </Show>

      {/* Staged attachment chips. */}
      <Show when={props.attachments.files().length > 0}>
        <AttachmentGroup class="mb-2">
          <For each={props.attachments.files()}>
            {(pf) => {
              const st = () => uploadState(pf.clientId);
              return (
                <Attachment size="sm" state={st()} class="w-52">
                  <AttachmentMedia variant={pf.previewUrl ? "image" : "icon"}>
                    <Show when={pf.previewUrl} fallback={<FileText />}>
                      <img src={pf.previewUrl} alt={pf.name} />
                    </Show>
                  </AttachmentMedia>
                  <AttachmentContent>
                    <AttachmentTitle>{pf.name}</AttachmentTitle>
                    <AttachmentDescription>
                      <Show when={st() === "error"} fallback={formatBytes(pf.size)}>
                        Upload failed
                      </Show>
                    </AttachmentDescription>
                  </AttachmentContent>
                  <AttachmentActions>
                    <Show when={st() === "uploading"}>
                      <LoaderCircle class="size-4 animate-spin text-muted-foreground" />
                    </Show>
                    <Show when={st() === "error"}>
                      <AttachmentAction
                        aria-label="Retry upload"
                        onClick={() => props.attachments.retry(pf.clientId)}
                      >
                        <RotateCcw />
                      </AttachmentAction>
                    </Show>
                    <Show when={st() !== "uploading"}>
                      <AttachmentAction
                        aria-label="Remove attachment"
                        onClick={() => props.attachments.removeFile(pf.clientId)}
                      >
                        <X />
                      </AttachmentAction>
                    </Show>
                  </AttachmentActions>
                </Attachment>
              );
            }}
          </For>
        </AttachmentGroup>
      </Show>

      <div class="relative flex items-end gap-2">
        {/* @-mention picker. Desktop: a keyboard-first popup above the composer
            (Up/Down move · Enter/Tab accept · Esc dismiss). Phone: a
            thumb-friendly horizontal chip row of open Pings. Both surface each
            Ping's @handle (mono) with its one-line description, and both float
            just above the input so they never push the composer. */}
        <Show when={mentions.open() && mentions.candidates().length > 0}>
          <Show
            when={!coarse()}
            fallback={
              <div
                data-slot="mention-chips"
                class="no-scrollbar absolute inset-x-0 bottom-full mb-2 flex snap-x gap-1.5 overflow-x-auto pr-6 pb-1 [mask-image:linear-gradient(to_right,#000_calc(100%-2rem),transparent)]"
                role="listbox"
                aria-label="Mention a Ping"
              >
                <For each={mentions.candidates()}>
                  {(ping) => (
                    <button
                      type="button"
                      role="option"
                      aria-selected={false}
                      data-slot="mention-chip"
                      class="flex shrink-0 snap-start items-center gap-1.5 rounded-full border border-border bg-card px-3 py-1.5 text-left"
                      onMouseDown={(e) => e.preventDefault()}
                      onClick={() => mentions.accept(ping)}
                    >
                      <AtSign class="size-3.5 shrink-0 text-muted-foreground" aria-hidden="true" />
                      <span class="font-mono text-xs text-foreground">{ping.name}</span>
                      <Show when={ping.description.trim().length > 0}>
                        <span class="max-w-[9rem] truncate text-[0.7rem] text-muted-foreground">
                          {ping.description}
                        </span>
                      </Show>
                    </button>
                  )}
                </For>
              </div>
            }
          >
            <div
              data-slot="mention-popup"
              class="absolute inset-x-0 bottom-full z-30 mb-2 max-h-64 overflow-y-auto rounded-md border border-border bg-card p-1 shadow-lg ring-1 ring-foreground/10"
              role="listbox"
              aria-label="Mention a Ping"
            >
              <For each={mentions.candidates()}>
                {(ping, i) => (
                  <button
                    type="button"
                    role="option"
                    data-slot="mention-option"
                    aria-selected={i() === mentions.activeIndex()}
                    class="flex w-full items-baseline gap-2 rounded px-2 py-1.5 text-left transition-colors"
                    classList={{
                      "bg-muted": i() === mentions.activeIndex(),
                      "hover:bg-muted/60": i() !== mentions.activeIndex(),
                    }}
                    onMouseDown={(e) => e.preventDefault()}
                    onMouseEnter={() => mentions.setActiveIndex(i())}
                    onClick={() => mentions.accept(ping)}
                  >
                    <span class="shrink-0 font-mono text-xs text-foreground">
                      @{ping.name}
                    </span>
                    <Show when={ping.description.trim().length > 0}>
                      <span class="min-w-0 flex-1 truncate text-xs text-muted-foreground">
                        {ping.description}
                      </span>
                    </Show>
                    <Show when={ping.requires_response}>
                      <span class="shrink-0 text-[0.62rem] uppercase tracking-[0.03em] text-primary">
                        needs you
                      </span>
                    </Show>
                  </button>
                )}
              </For>
            </div>
          </Show>
        </Show>

        <input
          ref={fileInputRef}
          type="file"
          multiple
          class="hidden"
          onChange={(e) => {
            if (e.currentTarget.files) props.attachments.addFiles(e.currentTarget.files);
            e.currentTarget.value = "";
          }}
        />
        <Button
          type="button"
          variant="ghost"
          size="icon"
          class="shrink-0 rounded-full text-muted-foreground"
          classList={{ "size-11": coarse() }}
          aria-label="Attach files"
          onClick={() => fileInputRef?.click()}
        >
          <Paperclip class="size-5" />
        </Button>
        <Textarea
          ref={(node: HTMLTextAreaElement) => {
            setRef(node);
            textareaRef = node;
          }}
          rows={1}
          data-composer="main"
          class="max-h-28 min-h-0 flex-1 resize-none py-2 leading-snug"
          placeholder="Message the Agent…"
          aria-label="Message the Agent"
          value={value()}
          onInput={(e) => {
            setValue(e.currentTarget.value);
            mentions.sync();
          }}
          onKeyUp={(e) => {
            // Caret moves (arrows/Home/End) can move INTO a mention while the
            // picker is closed — re-evaluate then. While it is open the picker
            // already owns the arrows (and keeps its own active-row state), so
            // don't re-sync and clobber it.
            if (
              !mentions.open() &&
              (e.key.startsWith("Arrow") || e.key === "Home" || e.key === "End")
            ) {
              mentions.sync();
            }
          }}
          onClick={() => mentions.sync()}
          onFocus={() => setFocused(true)}
          onBlur={() => {
            setFocused(false);
            mentions.close();
          }}
          onKeyDown={handleKeyDown}
          onPaste={handlePaste}
        />
        <Show when={props.thinking}>
          <Button
            type="button"
            variant="secondary"
            size="icon"
            class="shrink-0 rounded-full"
            classList={{ "size-11": coarse() }}
            aria-label="Stop the agent"
            onClick={() => props.onStop()}
          >
            <Square class="size-4 fill-current" />
          </Button>
        </Show>
        {/* Send + a labeled overflow for the otherwise-hidden queue action. The
            round Send stays the primary target (tap = send; long-press on touch
            still queues); the small caret exposes "Queue for next turn" — the
            desktop Tab shortcut — as a discoverable, labeled affordance so it is
            reachable by touch and by anyone who never learns the gesture. */}
        <div class="flex shrink-0 items-end gap-0.5">
          <Button
            type="button"
            size="icon"
            class="shrink-0 rounded-full"
            classList={{ "size-11": coarse() }}
            onPointerDown={onSendPointerDown}
            onPointerUp={onSendPointerUp}
            onPointerLeave={onSendPointerUp}
            onClick={onSendClick}
            disabled={!canSend() || sending()}
            aria-label="Send"
          >
            <Show when={sending()} fallback={<ArrowUp class="size-5" />}>
              <LoaderCircle class="size-5 animate-spin" />
            </Show>
          </Button>
          <DropdownMenu placement="top-end">
            <DropdownMenuTrigger
              class="flex shrink-0 items-center justify-center rounded-full text-muted-foreground transition-colors hover:text-foreground disabled:pointer-events-none disabled:opacity-40"
              classList={{ "size-11": coarse(), "size-7": !coarse() }}
              aria-label="More send options"
              disabled={!canSend() || sending()}
            >
              <ChevronDown class="size-4" />
            </DropdownMenuTrigger>
            <DropdownMenuContent>
              <DropdownMenuItem onSelect={() => void submit("send")}>Send now</DropdownMenuItem>
              <DropdownMenuItem onSelect={() => void submit("next_turn")}>
                Queue for next turn
              </DropdownMenuItem>
            </DropdownMenuContent>
          </DropdownMenu>
        </div>
      </div>

      {/* Bottom hint row: the keyboard hint surfaces only while the composer is
          focused (not standing chrome), and the offline cue mirrors the Side
          Chat wrap-up bar's "reconnect" pattern. The row only takes vertical
          space when it has something to say. */}
      <Show when={(focused() && !coarse()) || offline()}>
        <div class="mt-1 flex items-center gap-2 px-1">
          <Show when={focused() && !coarse()}>
            <div class="min-w-0 flex-1 truncate text-[0.66rem] text-muted-foreground/70">
              <span class="font-medium">Enter</span> send ·{" "}
              <span class="font-medium">Shift+Enter</span> newline ·{" "}
              <span class="font-medium">Tab</span> queue ·{" "}
              <span class="font-medium">Esc</span> stop ·{" "}
              <span class="font-medium">@</span> mention
            </div>
          </Show>
          <Show when={offline()}>
            <span class="ml-auto flex shrink-0 items-center gap-1 text-[0.66rem] text-status-attention">
              <span
                class="size-1.5 animate-pulse rounded-full bg-status-attention"
                aria-hidden="true"
              />
              offline · will queue
            </span>
          </Show>
        </div>
      </Show>
      </div>
    </div>
  );
}
