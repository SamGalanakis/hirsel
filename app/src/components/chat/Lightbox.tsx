import { X } from "lucide-solid";
import { createEffect, onCleanup, Show } from "solid-js";
import { Portal } from "solid-js/web";

interface Props {
  src: string | null;
  alt: string;
  onClose: () => void;
}

/** Full-screen image viewer. No image lib — just a fixed div with the image
 * centered; tap anywhere (or Esc) to dismiss. Pinch/scroll zoom is out of
 * scope per the spec. */
export function Lightbox(props: Props) {
  createEffect(() => {
    if (!props.src) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") props.onClose();
    };
    window.addEventListener("keydown", onKey);
    onCleanup(() => window.removeEventListener("keydown", onKey));
  });

  return (
    <Show when={props.src}>
      {(src) => (
        <Portal>
          <div
            class="fixed inset-0 z-[60] flex items-center justify-center bg-black/85 p-4 backdrop-blur-sm"
            role="dialog"
            aria-modal="true"
            aria-label={props.alt}
            onClick={() => props.onClose()}
          >
            <button
              type="button"
              class="absolute top-[calc(env(safe-area-inset-top)+0.75rem)] right-3 rounded-full bg-black/40 p-2 text-white/90 transition-colors hover:bg-black/60 hover:text-white"
              aria-label="Close"
              onClick={() => props.onClose()}
            >
              <X class="size-5" />
            </button>
            <img
              src={src()}
              alt={props.alt}
              class="max-h-full max-w-full rounded-lg object-contain"
              // Don't let a tap on the image itself bubble to the backdrop twice;
              // tapping the image should still dismiss (spec), so no stopPropagation.
            />
          </div>
        </Portal>
      )}
    </Show>
  );
}
