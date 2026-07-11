import { FileText } from "lucide-solid";
import { For, Show } from "solid-js";
import type { Blob } from "../../protocol";
import { blobUrl } from "../../ws/client";
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
 * agent bubbles and by history replay. */
export function MessageAttachments(props: Props) {
  return (
    <Show when={props.attachments && props.attachments.length > 0}>
      <div class="flex flex-col gap-2">
        <For each={props.attachments}>
          {(blob) => (
            <Show
              when={isImage(blob.mime)}
              fallback={
                <a
                  href={blobUrl(blob.id)}
                  target="_blank"
                  rel="noreferrer"
                  download={blob.name}
                  aria-label={`Download ${blob.name}`}
                  class="block no-underline"
                >
                  <Attachment size="sm" class="w-full max-w-xs">
                    <AttachmentMedia variant="icon">
                      <FileText />
                    </AttachmentMedia>
                    <AttachmentContent>
                      <AttachmentTitle>{blob.name}</AttachmentTitle>
                      <AttachmentDescription>{formatBytes(blob.size)}</AttachmentDescription>
                    </AttachmentContent>
                  </Attachment>
                </a>
              }
            >
              <button
                type="button"
                class="block overflow-hidden rounded-lg border border-border/60 outline-none focus-visible:ring-2 focus-visible:ring-ring"
                onClick={() => props.onOpenImage(blobUrl(blob.id), blob.name)}
              >
                <img
                  src={blobUrl(blob.id)}
                  alt={blob.name}
                  loading="lazy"
                  class="max-h-64 w-auto max-w-full object-contain"
                />
              </button>
            </Show>
          )}
        </For>
      </div>
    </Show>
  );
}
