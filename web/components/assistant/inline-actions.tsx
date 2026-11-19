"use client";

import { Check, Copy, LoaderCircle, Sparkles, X } from "lucide-react";
import {
  type Ref,
  useEffect,
  useImperativeHandle,
  useRef,
  useState,
} from "react";
import { Button, IconButton } from "@/components/ui/button";
import { errorMessage } from "@/lib/api/client";
import { postEventStream } from "@/lib/api/sse";
import type { ChatEvent } from "@/lib/api/types/ChatEvent";
import type { InlineAction } from "@/lib/api/types/InlineAction";

export type Selection = { from: number; to: number; text: string };

export type InlineActionsHandle = {
  /** Closes the result panel, stopping a request in progress. */
  close: () => void;
};

const ACTIONS: Array<{
  action: InlineAction;
  label: string;
  replaces: boolean;
}> = [
  { action: "rewrite", label: "Rewrite", replaces: true },
  { action: "shorten", label: "Shorten", replaces: true },
  { action: "fixGrammar", label: "Fix grammar", replaces: true },
  { action: "translate", label: "Translate…", replaces: true },
  { action: "explain", label: "Explain", replaces: false },
  { action: "custom", label: "Custom instruction…", replaces: true },
];

type Run = {
  label: string;
  selection: Selection;
  replaces: boolean;
  text: string;
  done: boolean;
  error: string | null;
};

/** The "AI" menu of the editor: quick actions on the selected text, and an image to LaTeX. */
export function InlineActions({
  projectId,
  getSelection,
  onReplace,
  onNotice,
  onImageFile,
  onRun,
  ref,
}: {
  projectId: string;
  getSelection: () => Selection | null;
  onReplace: (selection: Selection, text: string) => boolean;
  onNotice: (message: string) => void;
  /** An image chosen with "Image to LaTeX…". */
  onImageFile: (file: File) => void;
  /** A quick action starts. */
  onRun?: () => void;
  ref?: Ref<InlineActionsHandle>;
}) {
  const [open, setOpen] = useState(false);
  const [run, setRun] = useState<Run | null>(null);
  const abort = useRef<AbortController | null>(null);
  const menu = useRef<HTMLDivElement>(null);
  const imagePicker = useRef<HTMLInputElement>(null);

  useEffect(() => {
    if (!open) return;
    const close = (event: MouseEvent) => {
      if (!menu.current?.contains(event.target as Node)) setOpen(false);
    };
    window.addEventListener("mousedown", close);
    return () => window.removeEventListener("mousedown", close);
  }, [open]);

  const start = async (choice: (typeof ACTIONS)[number]) => {
    setOpen(false);
    const selection = getSelection();
    if (!selection || !selection.text.trim()) {
      onNotice("Select some text in the editor first.");
      return;
    }
    let instruction: string | undefined;
    if (choice.action === "translate") {
      instruction =
        window.prompt("Translate into which language?", "English") ?? undefined;
      if (!instruction) return;
    } else if (choice.action === "custom") {
      instruction =
        window.prompt("What should be done with the selected text?") ??
        undefined;
      if (!instruction) return;
    }
    abort.current?.abort();
    const controller = new AbortController();
    abort.current = controller;
    onRun?.();
    setRun({
      label: choice.label.replace("…", ""),
      selection,
      replaces: choice.replaces,
      text: "",
      done: false,
      error: null,
    });
    try {
      await postEventStream<ChatEvent>(
        "/api/ai/inline",
        { projectId, action: choice.action, text: selection.text, instruction },
        (event) => {
          if (event.type === "textDelta") {
            setRun((current) =>
              current
                ? { ...current, text: current.text + event.text }
                : current,
            );
          } else if (event.type === "error") {
            setRun((current) =>
              current ? { ...current, error: event.message } : current,
            );
          }
        },
        controller.signal,
      );
    } catch (error) {
      if (!(error instanceof DOMException && error.name === "AbortError")) {
        setRun((current) =>
          current ? { ...current, error: errorMessage(error) } : current,
        );
      }
    } finally {
      setRun((current) => (current ? { ...current, done: true } : current));
    }
  };

  const close = () => {
    abort.current?.abort();
    setRun(null);
  };
  useImperativeHandle(ref, () => ({ close }));

  return (
    <>
      <div ref={menu} className="absolute top-1 right-1 z-10">
        <Button
          variant="ghost"
          className="h-7 px-2 text-xs"
          onClick={() => setOpen((v) => !v)}
          title="AI actions on the selected text, or an image to LaTeX"
        >
          <Sparkles className="size-3.5 text-accent" /> AI
        </Button>
        {open && (
          <ul className="absolute top-8 right-0 z-20 w-48 rounded-md border border-border bg-background py-1 text-sm shadow-lg">
            {ACTIONS.map((choice) => (
              <li key={choice.action}>
                <button
                  type="button"
                  onClick={() => void start(choice)}
                  className="w-full px-3 py-1.5 text-left hover:bg-panel"
                >
                  {choice.label}
                </button>
              </li>
            ))}
            <li className="mt-1 border-t border-border pt-1">
              <button
                type="button"
                onClick={() => {
                  setOpen(false);
                  imagePicker.current?.click();
                }}
                className="w-full px-3 py-1.5 text-left hover:bg-panel"
              >
                Image to LaTeX…
              </button>
            </li>
          </ul>
        )}
        <input
          ref={imagePicker}
          type="file"
          accept="image/png,image/jpeg,image/webp"
          className="hidden"
          aria-hidden
          tabIndex={-1}
          onChange={(event) => {
            const file = event.target.files?.[0];
            event.target.value = "";
            if (file) onImageFile(file);
          }}
        />
      </div>
      {run && (
        <div className="absolute inset-x-3 top-12 z-10 max-h-[60%] overflow-auto rounded-lg border border-border bg-background p-3 shadow-xl">
          <div className="mb-2 flex items-center gap-2 text-xs text-muted">
            <Sparkles className="size-3.5 text-accent" />
            <span className="font-medium text-foreground">{run.label}</span>
            {!run.done && <LoaderCircle className="size-3.5 animate-spin" />}
            <span className="flex-1" />
            <IconButton label="Close" className="size-6" onClick={close}>
              <X className="size-3.5" />
            </IconButton>
          </div>
          {run.error ? (
            <p className="text-xs text-danger">{run.error}</p>
          ) : (
            <p className="whitespace-pre-wrap font-mono text-xs leading-relaxed">
              {run.text || "…"}
            </p>
          )}
          {run.done && !run.error && run.text && (
            <div className="mt-3 flex gap-2">
              {run.replaces && (
                <Button
                  variant="primary"
                  className="text-xs"
                  onClick={() => {
                    if (onReplace(run.selection, run.text.trim())) setRun(null);
                    else
                      onNotice(
                        "The selected text changed meanwhile; copy the result instead.",
                      );
                  }}
                >
                  <Check className="size-3.5" /> Replace selection
                </Button>
              )}
              <Button
                className="text-xs"
                onClick={() =>
                  void navigator.clipboard.writeText(run.text.trim())
                }
              >
                <Copy className="size-3.5" /> Copy
              </Button>
            </div>
          )}
        </div>
      )}
    </>
  );
}
