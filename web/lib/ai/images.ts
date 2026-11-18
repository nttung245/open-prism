/**
 * Images waiting in the assistant's message box (captured from the PDF) and what is sent for
 * them. Pure helpers, testable without a browser.
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
  /** The PDF page it was captured from. */
  page?: number;
  /** Where SyncTeX places it in the sources. */
  source?: SourceRef;
};

/** `images` with `image` added at the end, unless there are already `MAX_IMAGES`. */
export function addImage(
  images: DraftImage[],
  image: DraftImage,
): { images: DraftImage[]; added: boolean } {
  if (images.length >= MAX_IMAGES) return { images, added: false };
  return { images: [...images, image], added: true };
}

/** "Page 2 · chapters/intro.tex:12", or less when the image has less. */
export function imageCaption(image: {
  page?: number | null;
  source?: SourceRef | null;
}): string {
  const parts: string[] = [];
  if (image.page) parts.push(`Page ${image.page}`);
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
