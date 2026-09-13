export const THREAD_ICON_MIMES = ["image/png", "image/jpeg", "image/webp"] as const;
export const MAX_THREAD_ICON_BYTES = 256 * 1024;
const EDGE = 256;

function encodedCanvas(canvas: HTMLCanvasElement, type: string, quality?: number): Promise<Blob | null> {
  return new Promise(resolve => canvas.toBlob(resolve, type, quality));
}

/** Center-crop and encode a browser-selected image to the Host's icon contract. */
export async function normalizeThreadIconImage(file: File): Promise<File> {
  if (!THREAD_ICON_MIMES.includes(file.type as (typeof THREAD_ICON_MIMES)[number])) {
    throw new Error("Choose a PNG, JPEG, or WebP image. SVG is not supported.");
  }
  const bitmap = await createImageBitmap(file);
  try {
    if (bitmap.width < 1 || bitmap.height < 1 || bitmap.width > 4096 || bitmap.height > 4096 || bitmap.width * bitmap.height > 16_777_216) {
      throw new Error("Image dimensions must fit within 4096 pixels per side and 16 megapixels.");
    }
    const canvas = document.createElement("canvas");
    canvas.width = EDGE;
    canvas.height = EDGE;
    const context = canvas.getContext("2d");
    if (!context) throw new Error("This browser cannot prepare images.");
    const edge = Math.min(bitmap.width, bitmap.height);
    context.drawImage(bitmap, (bitmap.width - edge) / 2, (bitmap.height - edge) / 2, edge, edge, 0, 0, EDGE, EDGE);
    for (const quality of [0.86, 0.72, 0.58, 0.44]) {
      const blob = await encodedCanvas(canvas, "image/webp", quality);
      if (blob?.type === "image/webp" && blob.size <= MAX_THREAD_ICON_BYTES) {
        return new File([blob], "thread-icon.webp", { type: "image/webp" });
      }
    }
    const png = await encodedCanvas(canvas, "image/png");
    if (png && png.size <= MAX_THREAD_ICON_BYTES) {
      return new File([png], "thread-icon.png", { type: "image/png" });
    }
    throw new Error("The prepared image is still larger than 256 KiB.");
  } finally {
    bitmap.close();
  }
}
