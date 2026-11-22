"use client";

import { X } from "lucide-react";
import { type ReactNode, useEffect, useId, useRef } from "react";
import { IconButton } from "./button";

/**
 * A modal `<dialog>` with a title and a close button. `onClose` runs on Escape, on the close
 * button, and whenever the dialog closes.
 */
export function Modal({
  open,
  onClose,
  title,
  description,
  children,
  wide = false,
}: {
  open: boolean;
  onClose: () => void;
  title: string;
  description?: ReactNode;
  children: ReactNode;
  wide?: boolean;
}) {
  const dialog = useRef<HTMLDialogElement>(null);
  const titleId = useId();

  useEffect(() => {
    const element = dialog.current;
    if (!element) return;
    if (open && !element.open) element.showModal();
    else if (!open && element.open) element.close();
    // Next.js keeps a page it leaves mounted but hidden: a modal left open there would block
    // every click on the next page.
    return () => {
      if (element.open) element.close();
    };
  }, [open]);

  return (
    <dialog
      ref={dialog}
      onClose={onClose}
      aria-labelledby={titleId}
      className={`m-auto w-[calc(100%-2rem)] ${wide ? "max-w-2xl" : "max-w-md"} rounded-lg border border-border bg-background text-foreground shadow-xl backdrop:bg-black/40`}
    >
      {open && (
        <div className="flex flex-col gap-4 p-5">
          <div className="flex items-start justify-between gap-4">
            <div>
              <h2 id={titleId} className="text-base font-semibold">
                {title}
              </h2>
              {description && (
                <p className="mt-1 text-xs text-muted">{description}</p>
              )}
            </div>
            <IconButton label="Close" onClick={onClose}>
              <X className="size-4" />
            </IconButton>
          </div>
          {children}
        </div>
      )}
    </dialog>
  );
}

/** A labelled text field, as the dialogs use it. */
export const FIELD =
  "rounded-md border border-border bg-background px-2.5 py-1.5 text-sm outline-none focus:border-accent";
