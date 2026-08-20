import {
  ArrowUp,
  ChevronDown,
  CornerDownLeft,
  File as FileIcon,
  FileText,
  LoaderCircle,
  Paperclip,
  RotateCcw,
  Square,
  X,
} from "lucide-solid";
import { createEffect, createSignal, For, Show } from "solid-js";
import type { Blob, SendMode } from "../../protocol";
import { state } from "../../store/store";
import { anyOverlayOpen } from "../../lib/focus";
import { formatBytes } from "../../lib/format";
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
import type { AttachmentsController, PendingFile } from "./useAttachments";
import { extractTransferFiles, isLargePaste } from "./paste";
import { createFileDrop } from "./useFileDrop";

const MAX_HEIGHT_PX = 112;
const LONG_PRESS_MS = 450;
interface Props {
  attachments: AttachmentsController;
  thinking: boolean;
  /** One-shot composer pre-fill (v1.4 "Ask Hirsel to stop"); consumed once then cleared. */
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
  /** Focus is expressed by the surrounding field, never by composer copy. */
  focused?: boolean;
}

/** Composer anchored at the bottom of the task world. CLI-grade keyboard map on fine-pointer
 * devices (Enter send · Shift+Enter newline · Tab queue next-turn · Esc cancel
 * turn · ArrowUp recall); phone keeps Enter as newline and uses the send button
 * (long-press = queue). Handles attachment staging (paperclip + paste). */
export function Composer(props: Props) {
  // Shared input mechanics (value signal, coarse-pointer detection, auto-grow)
  // with any future constrained compact input.
  const { value, setValue, coarse, setRef, focus, caretToEnd } = useTextInput(MAX_HEIGHT_PX, "main");
  const [sending, setSending] = createSignal(false);
  const offline = () => state.connection !== "connected";
  let fileInputRef: HTMLInputElement | undefined;
  let longPressTimer: ReturnType<typeof setTimeout> | undefined;
  let longPressed = false;

  // Consume a one-shot pre-fill (v1.4 "Ask Hirsel to stop"): drop the text into the
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

    props.onSend(body, null, mode, blobs, []);
    props.attachments.clear();
    setValue("");
    focus();
  }

  function handleKeyDown(e: KeyboardEvent) {
    // An open overlay owns Esc first; gate stop-on-Esc on
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

  // Clipboard routing, in priority order: files (screenshots, copied images)
  // become chips; a paste too large to live in a four-line pill becomes a
  // pasted-text chip; everything else takes the browser's own insertion path
  // untouched, so ordinary pasting never changes behaviour.
  function handlePaste(e: ClipboardEvent) {
    const data = e.clipboardData;
    if (!data) return;
    const { files, rejected } = extractTransferFiles(data);
    if (files.length > 0) {
      e.preventDefault();
      for (const reason of new Set(rejected)) toast(reason, { variant: "error" });
      props.attachments.addPastedFiles(files);
      return;
    }
    const text = data.getData("text/plain");
    if (text && isLargePaste(text)) {
      e.preventDefault();
      props.attachments.addPastedText(text);
    }
  }

  // The escape hatch for auto-attaching: put the paste back into the field.
  // Peer clients that convert pastes without one ("Show in text field" is
  // ChatGPT's) are the single loudest complaint about the pattern.
  function insertAsText(clientId: string) {
    const text = props.attachments.takeText(clientId);
    if (text === null) return;
    const current = value();
    setValue(current.length > 0 ? `${current}\n${text}` : text);
    focus();
    caretToEnd();
  }

  const dragging = createFileDrop((data) => props.attachments.addFromTransfer(data));

  // One description line per chip kind: a paste is measured in lines (the thing
  // you actually want to know about it), a file in bytes.
  const describe = (pf: PendingFile) =>
    pf.lines === undefined ? formatBytes(pf.size) : `Pasted text · ${pf.lines} lines`;

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
    // The composer sits in the same frame as the field above it, so its left
    // edge lands on the field's left edge at every width — that shared edge is
    // what makes it read as the floor of the screen rather than a floating bar.
    <div class="mx-auto w-full max-w-frame flex-shrink-0 px-gutter pb-3 rail:pb-4">
    {/* One persistent organic capsule: the sole surface allowed the full pill
        signature because it is the stable transition between global and task.
        It has ONE width in every state — the reading measure it shares with the
        instrument and the conversation above it (DESIGN §4). Focus changes its
        tone only. The capsule used to narrow on focus, which moved it and its
        Send button ~300px sideways on every toggle; continuity of the one
        standing element beats the width distinction, and the ambient capsule
        was the odd one out anyway — wider than the conversation it sat under.
        It keeps a visible hairline at rest so the floor is legible before it is
        touched. */}
    <div
      data-slot="composer-shell"
      data-focused={props.focused ? "true" : "false"}
      data-dropping={dragging() ? "true" : "false"}
      class="w-full rounded-full px-3 py-2 ring-1 transition-[background-color,box-shadow] duration-200 ease-out rail:max-w-measure"
      classList={{
        // The drop state overrides both resting tones: while a file is in the
        // air the capsule is the one thing on screen that must read as a
        // target, so it takes the full mint ring regardless of focus.
        "ring-primary bg-primary/10": dragging(),
        "bg-primary/[0.035] ring-primary/25": props.focused && !dragging(),
        "bg-card/95 ring-border": !props.focused && !dragging(),
      }}
    >
      <div class="w-full">

      {/* Staged attachment chips. */}
      <Show when={props.attachments.files().length > 0}>
        <AttachmentGroup class="mb-2">
          <For each={props.attachments.files()}>
            {(pf) => {
              const st = () => uploadState(pf.clientId);
              // `title` carries the full name and, for a paste, its opening
              // lines — the chip itself stays one quiet row so several staged
              // items never push the capsule off the floor.
              return (
                <Attachment
                  size="sm"
                  state={st()}
                  class="w-52"
                  data-kind={pf.kind}
                  title={pf.text ? `${pf.name}\n\n${pf.text.slice(0, 400)}` : pf.name}
                >
                  <AttachmentMedia variant={pf.previewUrl ? "image" : "icon"}>
                    <Show
                      when={pf.previewUrl}
                      fallback={pf.kind === "file" ? <FileIcon /> : <FileText />}
                    >
                      <img src={pf.previewUrl} alt={pf.name} />
                    </Show>
                  </AttachmentMedia>
                  <AttachmentContent>
                    <AttachmentTitle>{pf.name}</AttachmentTitle>
                    <AttachmentDescription>
                      <Show when={st() === "error"} fallback={describe(pf)}>
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
                    <Show when={pf.text !== undefined && st() !== "uploading"}>
                      <AttachmentAction
                        aria-label={`Insert "${pf.name}" as text`}
                        onClick={() => insertAsText(pf.clientId)}
                      >
                        <CornerDownLeft />
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

        <input
          ref={(node) => { fileInputRef = node; }}
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
          }}
          rows={1}
          data-composer="main"
          class="max-h-28 min-h-11 flex-1 resize-none border-0 bg-transparent px-1 py-2 leading-snug shadow-none focus-visible:border-transparent focus-visible:ring-0 dark:bg-transparent"
          aria-label="Message Hirsel"
          value={value()}
          onInput={(e) => {
            setValue(e.currentTarget.value);
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

      {/* Bottom cue row: no standing keyboard-hint teaching (those keys live in
          the `?` shortcut sheet). Only the exceptional offline cue takes space,
          reserved for exceptional connection state. */}
      {/* Transient drop cue. It names the ceiling at the moment a file is in
          the air — the one moment the limit is actionable — so an over-cap
          file is a refusal the user was warned about, not a failed send. */}
      <Show when={dragging()}>
        <div class="mt-1 flex items-center px-1">
          <span class="text-xs text-primary">Drop to attach · up to 15 MB per file</span>
        </div>
      </Show>
      <Show when={offline() && !dragging()}>
        <div class="mt-1 flex items-center px-1">
          <span class="ml-auto flex shrink-0 items-center gap-1 text-xs text-status-attention">
            <span
              class="size-1.5 animate-pulse rounded-full bg-status-attention"
              aria-hidden="true"
            />
            offline · will queue
          </span>
        </div>
      </Show>
      </div>
    </div>
    </div>
  );
}
