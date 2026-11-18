/**
 * Images waiting in the assistant's message box and what is sent for them.
 * Pure helpers, testable without a browser.
 */
import type { ImageUpload } from "@/lib/api/types/ImageUpload";
import type { SourceRef } from "@/lib/api/types/SourceRef";

/** Most images in one message (the server refuses more). */
export const MAX_IMAGES = 4;

export type DraftImage = {
  /** Identifies the image in the list. */
  key: string;
  blob: Blob;
  /** Object URL of `blob` for the thumbnail; revoked when the image is removed or sent. */
  url: string;
  /** The file name, or a label for an image pasted from the clipboard. */
  name?: string;
  /** The PDF page it was captured from. */
  page?: number;
  /** Where SyncTeX places it in the sources. */
  source?: SourceRef;
};

/** Recognise the same file signatures as the server, regardless of the declared MIME type. */
export function sniffImageType(
  bytes: Uint8Array,
): "image/png" | "image/jpeg" | "image/webp" | null {
  if (
    [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a].every(
      (b, i) => bytes[i] === b,
    )
  ) {
    return "image/png";
  }
  if (bytes[0] === 0xff && bytes[1] === 0xd8 && bytes[2] === 0xff) {
    return "image/jpeg";
  }
  if (
    bytes[0] === 0x52 &&
    bytes[1] === 0x49 &&
    bytes[2] === 0x46 &&
    bytes[3] === 0x46 &&
    bytes[8] === 0x57 &&
    bytes[9] === 0x45 &&
    bytes[10] === 0x42 &&
    bytes[11] === 0x50
  ) {
    return "image/webp";
  }
  return null;
}

/** Fit the longest side within the limit without enlarging the image. */
export function fitWithin(width: number, height: number, max: number) {
  const scale = Math.min(1, max / Math.max(width, height));
  return {
    width: Math.max(1, Math.round(width * scale)),
    height: Math.max(1, Math.round(height * scale)),
  };
}

/** Add incoming images in order until the shared limit is reached. */
export function addImages(current: DraftImage[], incoming: DraftImage[]) {
  const added = Math.min(
    incoming.length,
    Math.max(0, MAX_IMAGES - current.length),
  );
  return {
    images: added ? [...current, ...incoming.slice(0, added)] : current,
    added,
    skipped: incoming.length - added,
  };
}

/** `images` with `image` added at the end, unless there are already `MAX_IMAGES`. */
export function addImage(
  images: DraftImage[],
  image: DraftImage,
): { images: DraftImage[]; added: boolean } {
  const result = addImages(images, [image]);
  return { images: result.images, added: result.added === 1 };
}

/** "Page 2 · chapters/intro.tex:12", or less when the image has less. */
export function imageCaption(image: {
  name?: string | null;
  page?: number | null;
  source?: SourceRef | null;
}): string {
  const parts: string[] = [];
  if (image.page) parts.push(`Page ${image.page}`);
  else if (image.name) parts.push(image.name);
  if (image.source) parts.push(`${image.source.file}:${image.source.line}`);
  return parts.join(" · ");
}

/** Base64 of some bytes, in chunks so large images do not overflow the call stack. */
export function bytesToBase64(bytes: Uint8Array): string {
  let binary = "";
  const chunk = 0x8000;
  for (let i = 0; i < bytes.length; i += chunk) {
    binary += String.fromCharCode(...bytes.subarray(i, i + chunk));
  }
  return btoa(binary);
}

/** What the server receives for the images of a message. */
export async function imageUploads(
  images: DraftImage[],
): Promise<ImageUpload[]> {
  return Promise.all(
    images.map(async (image) => ({
      data: bytesToBase64(new Uint8Array(await image.blob.arrayBuffer())),
      ...(image.page ? { page: image.page } : {}),
      ...(image.source ? { source: image.source } : {}),
    })),
  );
}
