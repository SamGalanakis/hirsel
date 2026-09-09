import type { RefTarget } from "../../lib/task-ref";
import {
  ArrowUp,
  CornerDownLeft,
  File as FileIcon,
  FileText,
  LoaderCircle,
  Paperclip,
  RotateCcw,
  Square,
  X,
} from "@/components/ui/icons";
import { createEffect, createSignal, For, Show } from "solid-js";

import type { Blob, SendMode } from "../../protocol";
import { state } from "../../store/store";
import { anyOverlayOpen } from "../../lib/focus";
import { formatBytes } from "../../lib/format";
import { handleSubmitKeys } from "../../lib/submitKeymap";
import { resolveMentionIds } from "../../lib/task-ref";
import { toast } from "../../lib/toast";
import { TASK_REF_PICKER_ID, TaskRefPicker } from "./TaskRefPicker";
import { createTaskRefPicker } from "./useTaskRefPicker";
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
import type { AttachmentsController, PendingFile } from "./useAttachments";
import { extractTransferFiles, isLargePaste } from "./paste";
import { createFileDrop } from "./useFileDrop";

const MAX_HEIGHT_PX = 112;
const LONG_PRESS_MS = 450;
interface Props {
  ariaLabel?: string;
  draftKey?: string;
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
  /** The citable field: every resting Task, in queue order. The `#` picker
   * offers these and the send resolves refs against them. */
  tasks?: RefTarget[];
}

/** Composer anchored at the bottom of the task world. CLI-grade keyboard map on fine-pointer
 * devices (Enter send · Shift+Enter newline · Cmd/Ctrl+Shift+Enter queue next-turn · Esc cancel
 * turn · ArrowUp recall); phone keeps Enter as newline and uses the send button
 * (long-press = queue). Handles attachment staging (paperclip + paste).
 *
 * Send stays visible for both pointer types. Enter sends on desktop; touch keeps
 * Enter as a newline. Queueing uses Ctrl/Cmd+Shift+Enter or a long press of Send.
 */
export function Composer(props: Props) {
  // Shared input mechanics (value signal, coarse-pointer detection, auto-grow)
  // with any future constrained compact input.
  const { value, setValue, coarse, setRef, focus, caretToEnd } = useTextInput(MAX_HEIGHT_PX, props.draftKey ?? "main");
  const [sending, setSending] = createSignal(false);
  const offline = () => state.connection !== "connected";
  let fileInputRef: HTMLInputElement | undefined;
  let textRef: HTMLTextAreaElement | undefined;
  // Typing `#` cites a Task. The picker owns the trigger, the caret anchor and
  // its own keyboard rung; the composed text stays the only record of what was
  // cited, so `mentions` is re-derived from the body at send time.
  const picker = createTaskRefPicker({
    getEl: () => textRef,
    value,
    setValue,
    tasks: () => props.tasks ?? [],
  });
  let longPressTimer: ReturnType<typeof setTimeout> | undefined;
  let longPressed = false;

  // Consume a one-shot pre-fill (v1.4 "Ask Hirsel to stop"): drop the text into the
  // draft, move the caret to the end, focus, then clear so it fires once.
  createEffect(() => props.prefill, (pre) => {
    if (!pre) return;
    setValue(pre);
    focus();
    caretToEnd();
    props.onConsumePrefill?.();
  });

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

    // The body IS the mention list: every `#id` still standing in the text at
    // send time, resolved against the live field. A deleted token drops its
    // mention for free, and a ref naming nothing is left as plain prose.
    props.onSend(body, null, mode, blobs, resolveMentionIds(body, props.tasks ?? []));
    props.attachments.clear();
    setValue("");
    picker.close();
    focus();
  }

  function handleKeyDown(e: KeyboardEvent) {
    // The picker gets the keys first, and only while it is open: arrows move the
    // active row, Enter/Tab accept it, Esc closes the picker and nothing else.
    if (picker.handleKeyDown(e)) return;
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
        onQueue: () => void submit("next_turn"),
        recallLast: props.getLastOwnerBody,
        onRecall: (text) => {
          setValue(text);
          caretToEnd();
        },
      })
    ) {
      return;
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

  /** The chip's hover text: the full name and, for a paste, its opening lines —
   * the chip itself stays one quiet row so several staged items never push the
   * capsule off the floor. A failed upload shows WHY on hover; the `error`
   * state carries its reason, so the chip no longer has to say only "failed". */
  const chipTitle = (pf: PendingFile): string => {
    const head = pf.text ? `${pf.name}\n\n${pf.text.slice(0, 400)}` : pf.name;
    return pf.upload.state === "error" ? `${head}\n\n${pf.upload.message}` : head;
  };

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
    // The composer sits in the same frame as the field above it, so its two
    // edges land on the conversation's two edges at every width — that shared
    // column is what makes it read as the floor of the screen rather than a
    // floating bar.
    <div class="mx-auto w-full max-w-frame flex-shrink-0 px-3 sm:px-gutter pb-3 rail:pb-4">
    {/* The same compact composer stays within the conversation's reading measure. */}
    <div
      data-slot="composer-shell"
      data-focused={props.focused ? "true" : "false"}
      data-dropping={dragging() ? "true" : "false"}
      class={["w-full rounded-xl px-2 py-1 ring-1 transition-[background-color,box-shadow] duration-200 ease-out", {
        // The drop state overrides both resting tones: while a file is in the
        // air the capsule is the one thing on screen that must read as a
        // target, so it takes the full mint ring regardless of focus.
        "ring-primary bg-primary/10": dragging(),
        "bg-primary/[0.035] ring-primary/25": !!props.focused && !dragging(),
        "bg-card/95 ring-border": !props.focused && !dragging(),
      }]}

    >
      <div class="w-full">

      {/* Staged attachment chips. */}
      <Show when={props.attachments.files().length > 0}>
        <AttachmentGroup class="mb-2">
          <For each={props.attachments.files()}>
            {(pf) => {
              // The chip's lifecycle now lives on the staged file itself, so
              // this is a plain field read instead of a join into a second store.
              const st = (): AttachmentState => pf.upload.state;
              return (
                <Attachment
                  size="sm"
                  state={st()}
                  class="w-52"
                  data-kind={pf.kind}
                  title={chipTitle(pf)}
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

      <div class="relative flex items-end gap-1">
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
          size="icon-sm"
          class={["shrink-0 rounded-full text-muted-foreground", { "size-11": coarse() }]}

          aria-label="Attach files"
          onClick={() => fileInputRef?.click()}
        >
          <Paperclip class={coarse() ? "size-5" : "size-4"} />
        </Button>
        <Textarea
          ref={(node: HTMLTextAreaElement) => {
            setRef(node);
            textRef = node;
          }}
          rows={1}
          data-composer="main"
          /* One line at rest. A fine pointer gets a 36px floor — the row is set
             by the text, not by a touch target — for a 44px capsule; a coarse
             pointer keeps the 44px one, so the capsule stays thumb-sized where
             thumbs use it. */
          class={`max-h-28 ${coarse() ? "min-h-11" : "min-h-9"} flex-1 resize-none border-0 bg-transparent px-1 py-1 leading-snug shadow-none focus-visible:border-transparent focus-visible:ring-0 dark:bg-transparent`}
          aria-label={props.ariaLabel ?? "Message Hirsel"}
          aria-expanded={(picker.open() ? true : undefined) ? "true" : "false"}
          aria-controls={picker.open() ? TASK_REF_PICKER_ID : undefined}
          aria-activedescendant={
            picker.open() && picker.activeIndex() >= 0
              ? `${TASK_REF_PICKER_ID}-option-${picker.candidates()[picker.activeIndex()].id}`
              : undefined
          }
          value={value()}
          onInput={(e) => {
            setValue(e.currentTarget.value);
            picker.sync(true);
          }}
          onKeyDown={handleKeyDown}
          /* The caret can also move without the text changing (arrows, a click,
             a selection): re-evaluate on those too, so stepping back into a
             half-typed `#dep` reopens the list the Owner left. */
          onKeyUp={() => picker.sync()}
          onClick={() => picker.sync()}
          onBlur={() => picker.close()}
          onPaste={handlePaste}
        />
        <TaskRefPicker
          candidates={picker.candidates()}
          activeIndex={picker.activeIndex()}
          anchorX={picker.anchor().x}
          onAccept={(task) => picker.accept(task)}
          onHover={picker.setActiveIndex}
        />
        {/* Stop stands on every pointer type while a turn is live — it is the
            one thing in the capsule Enter cannot do. */}
        <Show when={props.thinking}>
          <Button
            type="button"
            variant="secondary"
            size="icon-sm"
            class="size-11 shrink-0 rounded-full"

            aria-label="Stop the agent"
            onClick={() => props.onStop()}
          >
            <Square class={`fill-current ${coarse() ? "size-4" : "size-3.5"}`} />
          </Button>
        </Show>
        {/* Send stays visible on every pointer type. Enter remains the desktop
            shortcut; holding Send on touch queues the next turn. Stop remains
            alongside it while the Agent is working. */}
        <Button
          type="button"
          size="icon-sm"
          class="size-11 shrink-0 rounded-full"
          onPointerDown={onSendPointerDown}
          onPointerUp={onSendPointerUp}
          onPointerLeave={onSendPointerUp}
          onClick={onSendClick}
          disabled={!canSend() || sending()}
          aria-label="Send"
          title={coarse() ? "Send · hold to queue for next turn" : "Send · Ctrl/Cmd+Shift+Enter to queue for next turn"}
        >
          <Show when={sending()} fallback={<ArrowUp class="size-5" />}>
            <LoaderCircle class="size-5 animate-spin" />
          </Show>
        </Button>
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
