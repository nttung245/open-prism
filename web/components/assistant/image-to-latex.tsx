"use client";

import { Check, Copy, LoaderCircle, ScanText, X } from "lucide-react";
import { useCallback, useEffect, useRef, useState } from "react";
import { Button, IconButton } from "@/components/ui/button";
import { bytesToBase64 } from "@/lib/ai/images";
import { prepareImage } from "@/lib/ai/prepare-image";
import { errorMessage } from "@/lib/api/client";
import { type ImageLatexResult, streamImageLatex } from "@/lib/api/image-latex";

/** Where the LaTeX of an image goes: a remembered position in a file, and the text around it. */
export type ImageLatexTarget = {
  path: string;
  markId: string;
  before: string;
  after: string;
};

export type ImageLatexRun = {
  key: string;
  target: ImageLatexTarget;
  /** Object URL of the image, for the thumbnail. */
  imageUrl: string;
  /** The answer so far. */
  text: string;
  result: ImageLatexResult | null;
  error: string | null;
};

/**
 * Turns one image at a time into LaTeX: a new image replaces the run in progress. Nothing goes
 * into the document before `insert`, which hands the result to `onInsert`; a run closed or
 * replaced without inserting gives its target to `onDiscard`.
 */
export function useImageToLatex({
  projectId,
  onInsert,
  onDiscard,
}: {
  projectId: string;
  onInsert: (target: ImageLatexTarget, latex: string) => void;
  onDiscard: (target: ImageLatexTarget) => void;
}) {
  const [run, setRun] = useState<ImageLatexRun | null>(null);
  const current = useRef<{
    key: string;
    target: ImageLatexTarget;
    imageUrl: string;
    abort: AbortController;
  } | null>(null);
  const callbacks = useRef({ onInsert, onDiscard });
  callbacks.current = { onInsert, onDiscard };

  /** Stops the current run's request and frees its thumbnail. */
  const end = useCallback((discard: boolean) => {
    const previous = current.current;
    if (!previous) return;
    current.current = null;
    previous.abort.abort();
    URL.revokeObjectURL(previous.imageUrl);
    if (discard) callbacks.current.onDiscard(previous.target);
  }, []);
  useEffect(() => () => end(false), [end]);

  const start = useCallback(
    async (file: File, target: ImageLatexTarget) => {
      end(true);
      const key = crypto.randomUUID();
      const abort = new AbortController();
      const imageUrl = URL.createObjectURL(file);
      current.current = { key, target, imageUrl, abort };
      setRun({ key, target, imageUrl, text: "", result: null, error: null });
      const update = (change: Partial<ImageLatexRun>) =>
        setRun((run) => (run?.key === key ? { ...run, ...change } : run));
      try {
        const prepared = await prepareImage(file, file.name || "Image");
        const data = bytesToBase64(
          new Uint8Array(await prepared.blob.arrayBuffer()),
        );
        if (abort.signal.aborted) return;
        const result = await streamImageLatex(
          {
            projectId,
            path: target.path,
            image: { data },
            before: target.before,
            after: target.after,
          },
          abort.signal,
          (text) => update({ text }),
        );
        update({ result });
      } catch (error) {
        if (!abort.signal.aborted) update({ error: errorMessage(error) });
      }
    },
    [projectId, end],
  );

  const close = useCallback(() => {
    end(true);
    setRun(null);
  }, [end]);

  const insert = useCallback(() => {
    if (!run?.result?.latex || current.current?.key !== run.key) return;
    end(false);
    setRun(null);
    callbacks.current.onInsert(run.target, run.result.latex);
  }, [run, end]);

  return { run, start, close, insert };
}

/** The "Image → LaTeX" panel over the editor: the image, the LaTeX as it is written, and what
 * to do with it. */
export function ImageToLatex({
  run,
  onInsert,
  onClose,
}: {
  run: ImageLatexRun;
  onInsert: () => void;
  onClose: () => void;
}) {
  const { result } = run;
  const writing = !result && !run.error;
  return (
    <section
      aria-label="Image to LaTeX"
      className="absolute inset-x-3 top-12 z-10 max-h-[60%] overflow-auto rounded-lg border border-border bg-background p-3 shadow-xl"
    >
      <div className="mb-2 flex items-center gap-2 text-xs text-muted">
        <ScanText className="size-3.5 text-accent" />
        <span className="font-medium text-foreground">Image → LaTeX</span>
        {writing && <LoaderCircle className="size-3.5 animate-spin" />}
        <span className="flex-1" />
        <IconButton label="Close" className="size-6" onClick={onClose}>
          <X className="size-3.5" />
        </IconButton>
      </div>
      <div className="flex gap-3">
        {/* biome-ignore lint/performance/noImgElement: a local object URL, not optimisable. */}
        <img
          src={run.imageUrl}
          alt="The pasted content"
          className="max-h-24 max-w-32 shrink-0 self-start rounded border border-border bg-white object-contain"
        />
        <div className="min-w-0 flex-1">
          {run.error ? (
            <p className="text-xs text-danger">{run.error}</p>
          ) : result && !result.latex ? (
            <p className="text-xs text-muted">
              No LaTeX could be read from this image.
            </p>
          ) : (
            <pre className="whitespace-pre-wrap break-words font-mono text-xs leading-relaxed">
              {(result ? result.latex : run.text) || "…"}
            </pre>
          )}
        </div>
      </div>
      {result && result.packages.length > 0 && (
        <p className="mt-2 text-xs text-muted">
          Needs{" "}
          <code className="font-mono text-foreground">
            {result.packages.map((name) => `\\usepackage{${name}}`).join(" ")}
          </code>{" "}
          in the preamble.
        </p>
      )}
      {result?.citationsRemoved && (
        <p className="mt-2 text-xs text-muted">
          Citations were replaced with [?]; add them yourself.
        </p>
      )}
      {result?.latex && (
        <div className="mt-3 flex gap-2">
          <Button variant="primary" className="text-xs" onClick={onInsert}>
            <Check className="size-3.5" /> Insert
          </Button>
          <Button
            className="text-xs"
            onClick={() => void navigator.clipboard.writeText(result.latex)}
          >
            <Copy className="size-3.5" /> Copy
          </Button>
        </div>
      )}
    </section>
  );
}
