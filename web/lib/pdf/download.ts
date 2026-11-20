/** Downloading part of a compiled PDF: `GET …/runs/{runId}/pages?pages=3-7,10`. */

/** The pages URL of the run whose PDF is at `pdfUrl`, or `null` for any other URL. */
export function pagesUrl(pdfUrl: string): `/api/${string}` | null {
  const match =
    /^\/api\/projects\/([^/?#]+)\/runs\/([^/?#]+)\/output\.pdf$/.exec(pdfUrl);
  return match ? `/api/projects/${match[1]}/runs/${match[2]}/pages` : null;
}

/**
 * The file name of a `Content-Disposition` header: the UTF-8 `filename*` when present, else
 * the plain `filename`.
 */
export function fileNameFromDisposition(header: string | null): string | null {
  if (!header) return null;
  const extended = /filename\*\s*=\s*UTF-8''([^;]+)/i.exec(header);
  if (extended) {
    try {
      return decodeURIComponent(extended[1].trim());
    } catch {
      // Fall back to the plain name.
    }
  }
  const plain = /filename\s*=\s*"([^"]*)"/i.exec(header);
  return plain ? plain[1] : null;
}

/** Hands `blob` to the browser as a download named `name`. */
export function saveBlob(blob: Blob, name: string): void {
  const link = document.createElement("a");
  link.href = URL.createObjectURL(blob);
  link.download = name;
  link.click();
  // The download has started by the time the link is clicked; keep the URL briefly anyway.
  setTimeout(() => URL.revokeObjectURL(link.href), 10_000);
}
