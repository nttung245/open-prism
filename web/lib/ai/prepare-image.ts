import { fitWithin, sniffImageType } from "./images";

const MAX_SIDE = 2000;
const MAX_BYTES = 5 * 1024 * 1024;

/** Validate and, if necessary, resize an image before it is attached. */
export async function prepareImage(
  file: File,
  name: string,
): Promise<{ blob: Blob; name: string }> {
  const mediaType = sniffImageType(
    new Uint8Array(await file.slice(0, 12).arrayBuffer()),
  );
  if (!mediaType)
    throw new Error("Only PNG, JPEG or WebP images are supported");

  // Keep the original bytes even if the browser provided no or an incorrect MIME type.
  let blob: Blob =
    file.type === mediaType ? file : file.slice(0, file.size, mediaType);
  const bitmap = await createImageBitmap(blob);
  try {
    if (Math.max(bitmap.width, bitmap.height) > MAX_SIDE) {
      const size = fitWithin(bitmap.width, bitmap.height, MAX_SIDE);
      const canvas = document.createElement("canvas");
      canvas.width = size.width;
      canvas.height = size.height;
      const context = canvas.getContext("2d");
      if (!context) throw new Error("Could not resize the image");
      context.drawImage(bitmap, 0, 0, size.width, size.height);
      const outputType =
        mediaType === "image/jpeg" ? "image/jpeg" : "image/png";
      const resized = await new Promise<Blob | null>((resolve) =>
        canvas.toBlob(resolve, outputType, 0.9),
      );
      if (!resized) throw new Error("Could not resize the image");
      blob = resized;
    }
  } finally {
    bitmap.close();
  }

  if (blob.size > MAX_BYTES) throw new Error("Image is larger than 5 MB");
  return { blob, name };
}
