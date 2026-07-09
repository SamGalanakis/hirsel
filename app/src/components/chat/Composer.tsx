import { ArrowUp, FileText, LoaderCircle, Paperclip, RotateCcw, Square, X } from "lucide-solid";
import { createEffect, createSignal, For, Show } from "solid-js";
import type { Blob, SendMode } from "../../protocol";
import { state } from "../../store/store";
import type { DisplayMessage } from "../../store/types";
import { formatBytes } from "../../lib/format";
import { toast } from "../../lib/toast";
import { Button } from "../ui/button";
import { Textarea } from "../ui/textarea";
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

interface Props {
  replyingTo: DisplayMessage | undefined | null;
  onCancelReply: () => void;
  attachments: AttachmentsController;
  thinking: boolean;
  /** One-shot composer pre-fill (v1.4 "Ask to stop"); consumed once then cleared. */
  prefill?: string | null;
  onConsumePrefill?: () => void;
  onSend: (body: string, ref: number | null, mode: SendMode, blobs: Blob[]) => void;
  onStop: () => void;
  getLastOwnerBody: () => string | null;
}

function snippet(body: string): string {
  const oneLine = body.replace(/\s+/g, " ").trim();
  return oneLine.length > 60 ? `${oneLine.slice(0, 60)}…` : oneLine;
}

/** Composer anchored at the bottom of Chat, below the Tray shelf. CLI-grade keyboard map on fine-pointer
 * devices (Enter send · Shift+Enter newline · Tab queue next-turn · Esc cancel
 * turn · ArrowUp recall); phone keeps Enter as newline and uses the send button
 * (long-press = queue). Handles attachment staging (paperclip + paste). */
export function Composer(props: Props) {
  // Shared input mechanics (value signal, coarse-pointer detection, auto-grow)
  // with the Inbox inline ReplyInput.
  const { value, setValue, coarse, setRef, focus, caretToEnd } = useTextInput(MAX_HEIGHT_PX);
  const [sending, setSending] = createSignal(false);
  let fileInputRef: HTMLInputElement | undefined;
  let longPressTimer: ReturnType<typeof setTimeout> | undefined;
  let longPressed = false;

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

    props.onSend(body, props.replyingTo?.id ?? null, mode, blobs);
    props.attachments.clear();
    setValue("");
    if (props.replyingTo) props.onCancelReply();
    focus();
  }

  function handleKeyDown(e: KeyboardEvent) {
    // Esc interrupts the active turn (no-op if idle).
    if (e.key === "Escape") {
      if (props.thinking) {
        e.preventDefault();
        props.onStop();
      }
      return;
    }
    // ArrowUp on an empty composer recalls the last owner message.
    if (e.key === "ArrowUp" && value().length === 0) {
      const last = props.getLastOwnerBody();
      if (last !== null) {
        e.preventDefault();
        setValue(last);
        caretToEnd();
      }
      return;
    }
    // Ctrl/Cmd+Enter always sends, on every device.
    if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) {
      e.preventDefault();
      void submit("send");
      return;
    }
    if (coarse()) return; // phone: Enter stays a newline; send button submits.
    // Desktop:
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      void submit("send");
      return;
    }
    // Tab with a non-empty composer queues a next-turn message; empty Tab keeps
    // normal focus movement.
    if (e.key === "Tab" && !e.shiftKey && value().trim().length > 0) {
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
    <div class="flex-shrink-0 border-t border-border bg-card px-3 py-2">
      <Show when={props.replyingTo}>
        {(replyingTo) => (
          <div class="mb-2 flex items-start gap-2 rounded-md border-l-2 border-primary bg-black/15 px-2 py-1">
            <div class="min-w-0 flex-1">
              <div class="text-[0.68rem] uppercase tracking-[0.03em] text-primary">
                Replying to {replyingTo().author === "owner" ? "you" : "Agent"}
              </div>
              <div class="overflow-hidden text-ellipsis whitespace-nowrap text-xs text-muted-foreground">
                {snippet(replyingTo().body)}
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

      <div class="flex items-end gap-2">
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
          aria-label="Attach files"
          onClick={() => fileInputRef?.click()}
        >
          <Paperclip class="size-5" />
        </Button>
        <Textarea
          ref={setRef}
          rows={1}
          class="max-h-28 min-h-0 flex-1 resize-none py-2 leading-snug"
          placeholder="Message the Agent…"
          value={value()}
          onInput={(e) => setValue(e.currentTarget.value)}
          onKeyDown={handleKeyDown}
          onPaste={handlePaste}
        />
        <Show when={props.thinking}>
          <Button
            type="button"
            variant="secondary"
            size="icon"
            class="shrink-0 rounded-full"
            aria-label="Stop the agent"
            onClick={() => props.onStop()}
          >
            <Square class="size-4 fill-current" />
          </Button>
        </Show>
        <Button
          type="button"
          size="icon"
          class="shrink-0 rounded-full"
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
      </div>

      {/* Subtle desktop keyboard hint. */}
      <Show when={!coarse()}>
        <div class="mt-1 px-1 text-[0.66rem] text-muted-foreground/70">
          <span class="font-medium">Enter</span> send ·{" "}
          <span class="font-medium">Shift+Enter</span> newline ·{" "}
          <span class="font-medium">Tab</span> queue ·{" "}
          <span class="font-medium">Esc</span> stop
        </div>
      </Show>
    </div>
  );
}
