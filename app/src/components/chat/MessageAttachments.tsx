import { FileText, ImageOff } from "lucide-solid";
import { createSignal, For, onMount, Show } from "solid-js";
import type { Blob } from "../../protocol";
import { getClient } from "../../ws/client";
import { formatBytes } from "../../lib/format";
import {
  Attachment,
  AttachmentContent,
  AttachmentDescription,
  AttachmentMedia,
  AttachmentTitle,
} from "../ui/attachment";

interface Props {
  attachments: Blob[] | undefined;
  onOpenImage: (src: string, alt: string) => void;
}

function isImage(mime: string): boolean {
  return mime.startsWith("image/");
}

/** Renders a message's attachments: image/* inline (constrained height, tap →
 * lightbox), everything else as a downloadable file chip. Shared by owner and
 * agent bubbles and by history replay. Blob bytes are fetched through a
 * short-lived signed URL (D9) requested per attachment, so nothing here embeds a
 * token. */
export function MessageAttachments(props: Props) {
  return (
    <Show when={props.attachments && props.attachments.length > 0}>
      <div class="flex flex-col gap-2">
        <For each={props.attachments}>
          {(blob) => <AttachmentItem blob={blob} onOpenImage={props.onOpenImage} />}
        </For>
      </div>
    </Show>
  );
}

/** One attachment. Resolves its signed URL on mount (needed to render the
 * thumbnail / set the download href); the lightbox re-resolves on tap so the
 * full-size view always gets a fresh, un-expired URL. */
function AttachmentItem(props: { blob: Blob; onOpenImage: (src: string, alt: string) => void }) {
  const [url, setUrl] = createSignal<string | null>(null);
  const [failed, setFailed] = createSignal(false);

  onMount(() => {
    const client = getClient();
    if (!client) {
      setFailed(true);
      return;
    }
    client
      .getBlobUrl(props.blob.id)
      .then(setUrl)
      .catch(() => setFailed(true));
  });

  async function openLightbox() {
    // Prefer a fresh URL (the mount-time one may have expired if the message sat
    // on screen); fall back to the resolved one if a re-fetch fails.
    try {
      const fresh = (await getClient()?.getBlobUrl(props.blob.id)) ?? url();
      if (fresh) props.onOpenImage(fresh, props.blob.name);
    } catch {
      const current = url();
      if (current) props.onOpenImage(current, props.blob.name);
    }
  }

  return (
    <Show
      when={isImage(props.blob.mime)}
      fallback={
        <Show
          when={url()}
          fallback={
            <FileChip blob={props.blob} failed={failed()} />
          }
        >
          {(href) => (
            <a
              href={href()}
              target="_blank"
              rel="noreferrer"
              download={props.blob.name}
              aria-label={`Download ${props.blob.name}`}
              class="block no-underline"
            >
              <FileChip blob={props.blob} failed={false} />
            </a>
          )}
        </Show>
      }
    >
      <Show
        when={url()}
        fallback={
          <div class="flex h-40 w-full max-w-xs items-center justify-center rounded-lg border border-border/60 bg-muted text-muted-foreground">
            <Show
              when={failed()}
              fallback={<span class="text-xs">Loading…</span>}
            >
              <span class="flex items-center gap-1.5 text-xs">
                <ImageOff class="size-4" aria-hidden="true" />
                Couldn't load image
              </span>
            </Show>
          </div>
        }
      >
        {(src) => (
          <button
            type="button"
            class="block overflow-hidden rounded-lg border border-border/60 outline-none focus-visible:ring-2 focus-visible:ring-ring"
            onClick={openLightbox}
          >
            <img
              src={src()}
              alt={props.blob.name}
              loading="lazy"
              class="max-h-64 w-auto max-w-full object-contain"
            />
          </button>
        )}
      </Show>
    </Show>
  );
}

/** The non-image file chip (also the loading/failed placeholder for a file
 * whose URL hasn't resolved). */
function FileChip(props: { blob: Blob; failed: boolean }) {
  return (
    <Attachment size="sm" class="w-full max-w-xs" state={props.failed ? "error" : undefined}>
      <AttachmentMedia variant="icon">
        <FileText />
      </AttachmentMedia>
      <AttachmentContent>
        <AttachmentTitle>{props.blob.name}</AttachmentTitle>
        <AttachmentDescription>
          <Show when={props.failed} fallback={formatBytes(props.blob.size)}>
            Couldn't load
          </Show>
        </AttachmentDescription>
      </AttachmentContent>
    </Attachment>
  );
}
