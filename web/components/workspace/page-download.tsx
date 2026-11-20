"use client";

import { FileDown, LoaderCircle } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { Button, IconButton } from "@/components/ui/button";
import { apiFetch, errorMessage } from "@/lib/api/client";
import {
  fileNameFromDisposition,
  pagesUrl,
  saveBlob,
} from "@/lib/pdf/download";

/**
 * "Download pages…" next to the PDF's download button: a small form for a page list such as
 * "3-7, 10", filled in with the page being read. The server checks the list and names the
 * file; its error shows next to the field.
 */
export function PageDownload({
  pdfUrl,
  currentPage,
  disabled,
}: {
  /** `…/runs/{runId}/output.pdf` of the PDF on screen. */
  pdfUrl: string;
  currentPage: number;
  disabled: boolean;
}) {
  const [open, setOpen] = useState(false);
  const [pages, setPages] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const root = useRef<HTMLDivElement>(null);
  const url = pagesUrl(pdfUrl);

  useEffect(() => {
    if (!open) return;
    const close = (event: MouseEvent) => {
      if (!root.current?.contains(event.target as Node)) setOpen(false);
    };
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") setOpen(false);
    };
    window.addEventListener("mousedown", close);
    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("mousedown", close);
      window.removeEventListener("keydown", onKey);
    };
  }, [open]);

  if (!url) return null;

  const toggle = () => {
    if (!open) {
      setPages(String(currentPage));
      setError(null);
    }
    setOpen(!open);
  };

  const download = async () => {
    setBusy(true);
    setError(null);
    try {
      const response = await apiFetch(
        `${url}?pages=${encodeURIComponent(pages)}`,
      );
      const name =
        fileNameFromDisposition(response.headers.get("content-disposition")) ??
        "pages.pdf";
      saveBlob(await response.blob(), name);
      setOpen(false);
    } catch (problem) {
      setError(errorMessage(problem));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div ref={root} className="relative">
      <IconButton
        label="Download pages…"
        aria-haspopup="dialog"
        aria-expanded={open}
        onClick={toggle}
        disabled={disabled}
      >
        <FileDown className="size-4" />
      </IconButton>
      {open && (
        <form
          aria-label="Download pages"
          onSubmit={(event) => {
            event.preventDefault();
            void download();
          }}
          className="absolute top-8 right-0 z-30 flex w-64 flex-col gap-2 rounded-md border border-border bg-background p-3 text-xs shadow-lg"
        >
          <label className="flex flex-col gap-1 text-muted">
            Pages
            <input
              value={pages}
              onChange={(event) => setPages(event.target.value)}
              placeholder="3-7, 10"
              // biome-ignore lint/a11y/noAutofocus: the field is the only thing the popup is for.
              autoFocus
              className="rounded-md border border-border bg-background px-2 py-1 text-sm text-foreground outline-none focus:border-accent"
            />
          </label>
          {error && (
            <p role="alert" className="text-danger">
              {error}
            </p>
          )}
          <Button
            type="submit"
            variant="primary"
            disabled={busy}
            className="self-end"
          >
            {busy && <LoaderCircle className="size-3.5 animate-spin" />}
            Download
          </Button>
        </form>
      )}
    </div>
  );
}
