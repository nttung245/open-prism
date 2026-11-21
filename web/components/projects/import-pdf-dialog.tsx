"use client";

import { useMutation } from "@tanstack/react-query";
import { FileUp, LoaderCircle, X } from "lucide-react";
import Link from "next/link";
import { type SubmitEvent, useEffect, useRef, useState } from "react";
import { Button, IconButton } from "@/components/ui/button";
import { ApiError, apiJson, errorMessage } from "@/lib/api/client";
import type { JobView } from "@/lib/api/types/JobView";

const INPUT =
  "w-full min-w-0 rounded-md border border-border bg-background px-3 py-1.5 text-sm outline-none focus:border-accent";

/** `MAX_PDF_BYTES` of the API (`crates/core/src/pdf_pages.rs`). Checked here first: the API
 * refuses a larger upload while it is still being sent, which the page sees as a network error. */
const MAX_PDF_MB = 200;

function stem(fileName: string): string {
  return fileName.replace(/\.pdf$/i, "");
}

/**
 * Chooses a PDF, the name of the new project and the pages to import, then starts the import
 * job. Errors of the API show here, the missing Agent model with a link to Settings.
 */
export function ImportPdfDialog({
  open,
  onClose,
  onStarted,
}: {
  open: boolean;
  onClose: () => void;
  onStarted: (job: JobView) => void;
}) {
  const dialog = useRef<HTMLDialogElement>(null);
  const form = useRef<HTMLFormElement>(null);
  const [file, setFile] = useState<File | null>(null);
  const [name, setName] = useState("");
  const [pages, setPages] = useState("");

  const start = useMutation({
    mutationFn: async (chosen: File) => {
      const form = new FormData();
      if (name.trim()) form.append("name", name.trim());
      if (pages.trim()) form.append("pages", pages.trim());
      form.append("file", chosen);
      return apiJson<JobView>("/api/imports/pdf", {
        method: "POST",
        body: form,
      });
    },
    onSuccess: (job) => {
      onStarted(job);
      onClose();
    },
  });
  const { reset } = start;

  useEffect(() => {
    const element = dialog.current;
    if (!element) return;
    if (open && !element.open) {
      // The file input keeps its file across openings unless the form is reset.
      form.current?.reset();
      setFile(null);
      setName("");
      setPages("");
      reset();
      element.showModal();
    } else if (!open && element.open) {
      element.close();
    }
  }, [open, reset]);

  const onSubmit = (event: SubmitEvent<HTMLFormElement>) => {
    event.preventDefault();
    if (file) start.mutate(file);
  };

  const noModel =
    start.error instanceof ApiError && start.error.code === "no_model";
  const tooLarge = file !== null && file.size > MAX_PDF_MB * 1024 * 1024;

  return (
    <dialog
      ref={dialog}
      onClose={onClose}
      aria-labelledby="import-pdf-title"
      className="m-auto w-[calc(100%-2rem)] max-w-md rounded-lg border border-border bg-background text-foreground shadow-xl backdrop:bg-black/40"
    >
      <form ref={form} onSubmit={onSubmit} className="flex flex-col gap-4 p-5">
        <div className="flex items-start justify-between gap-4">
          <div>
            <h2 id="import-pdf-title" className="text-base font-semibold">
              Import PDF
            </h2>
            <p className="mt-1 text-xs text-muted">
              The Agent model reads each page and writes it as LaTeX in a new
              project. Each page is sent to it as an image, with its text. A
              3-page paper takes 30–60 seconds.
            </p>
          </div>
          <IconButton label="Close" onClick={onClose}>
            <X className="size-4" />
          </IconButton>
        </div>

        <label className="flex flex-col gap-1 text-sm">
          <span className="font-medium">PDF file</span>
          <input
            type="file"
            accept=".pdf,application/pdf"
            required
            autoFocus
            className="text-sm file:mr-3 file:rounded-md file:border file:border-border file:bg-background file:px-2.5 file:py-1 file:text-sm hover:file:bg-panel"
            onChange={(event) => {
              setFile(event.target.files?.[0] ?? null);
              reset();
            }}
          />
        </label>

        <label className="flex flex-col gap-1 text-sm">
          <span className="font-medium">Project name</span>
          <input
            value={name}
            onChange={(event) => setName(event.target.value)}
            placeholder={file ? stem(file.name) : "The name of the file"}
            maxLength={200}
            className={INPUT}
          />
        </label>

        <label className="flex flex-col gap-1 text-sm">
          <span className="font-medium">Pages</span>
          <input
            value={pages}
            onChange={(event) => setPages(event.target.value)}
            placeholder="All pages, or for example 1-5, 8"
            className={INPUT}
          />
        </label>

        {tooLarge && (
          <p role="alert" className="text-sm text-danger">
            This PDF is larger than {MAX_PDF_MB} MB, the most Open Prism
            imports.
          </p>
        )}
        {start.error && (
          <p role="alert" className="text-sm text-danger">
            {errorMessage(start.error)}
            {noModel && (
              <>
                {" "}
                <Link href="/settings" className="text-accent underline">
                  Open Settings
                </Link>
              </>
            )}
          </p>
        )}

        <div className="flex justify-end gap-2">
          <Button onClick={onClose}>Cancel</Button>
          <Button
            type="submit"
            variant="primary"
            disabled={!file || tooLarge || start.isPending}
          >
            {start.isPending ? (
              <LoaderCircle className="size-4 animate-spin" />
            ) : (
              <FileUp className="size-4" />
            )}
            Import
          </Button>
        </div>
      </form>
    </dialog>
  );
}
