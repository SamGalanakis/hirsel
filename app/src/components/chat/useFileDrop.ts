import { createSignal, onCleanup, onMount } from "solid-js";

/** Window-scoped file drag tracking with one drop sink.
 *
 * The composer is a thin pill at the floor of the screen — a poor dart board for
 * a dragged file. So the whole window accepts the drop while the composer shows
 * the target state: aim anywhere, the attachment still lands where attachments
 * live. Without a window-level handler the browser's own default wins and a
 * stray drop navigates away from the app, losing the draft.
 *
 * `dragenter`/`dragleave` fire once per element crossed, so a depth counter is
 * what keeps the state from flickering off as the pointer moves between
 * children. */
export function createFileDrop(onDrop: (data: DataTransfer | null) => void) {
  const [dragging, setDragging] = createSignal(false);
  let depth = 0;

  const carriesFiles = (e: DragEvent) => Array.from(e.dataTransfer?.types ?? []).includes("Files");

  const onDragEnter = (e: DragEvent) => {
    if (!carriesFiles(e)) return;
    depth += 1;
    setDragging(true);
  };
  const onDragLeave = (e: DragEvent) => {
    if (!carriesFiles(e)) return;
    depth = Math.max(0, depth - 1);
    if (depth === 0) setDragging(false);
  };
  const onDragOver = (e: DragEvent) => {
    if (!carriesFiles(e)) return;
    e.preventDefault(); // without this the drop event never fires
    if (e.dataTransfer) e.dataTransfer.dropEffect = "copy";
  };
  const onDropEvent = (e: DragEvent) => {
    if (!carriesFiles(e)) return;
    e.preventDefault();
    depth = 0;
    setDragging(false);
    onDrop(e.dataTransfer);
  };

  onMount(() => {
    window.addEventListener("dragenter", onDragEnter);
    window.addEventListener("dragleave", onDragLeave);
    window.addEventListener("dragover", onDragOver);
    window.addEventListener("drop", onDropEvent);
  });
  onCleanup(() => {
    window.removeEventListener("dragenter", onDragEnter);
    window.removeEventListener("dragleave", onDragLeave);
    window.removeEventListener("dragover", onDragOver);
    window.removeEventListener("drop", onDropEvent);
  });

  return dragging;
}
