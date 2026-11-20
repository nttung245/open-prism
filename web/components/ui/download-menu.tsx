"use client";

import { Download } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { IconButton } from "./button";

const FORMATS = [
  { format: "zip", label: "ZIP" },
  { format: "7z", label: "7z" },
] as const;

/** The project's download button: a small menu of archive formats. */
export function DownloadMenu({ projectId }: { projectId: string }) {
  const [open, setOpen] = useState(false);
  const root = useRef<HTMLDivElement>(null);

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

  return (
    <div ref={root} className="relative">
      <IconButton
        label="Download project"
        aria-haspopup="menu"
        aria-expanded={open}
        onClick={() => setOpen((value) => !value)}
      >
        <Download className="size-4" />
      </IconButton>
      {open && (
        <div
          role="menu"
          aria-label="Download project"
          className="absolute top-8 right-0 z-30 flex w-32 flex-col rounded-md border border-border bg-background py-1 text-sm shadow-lg"
        >
          {FORMATS.map(({ format, label }) => (
            <a
              key={format}
              role="menuitem"
              href={`/api/projects/${projectId}/export?format=${format}`}
              onClick={() => setOpen(false)}
              className="px-3 py-1.5 hover:bg-panel"
            >
              {label}
            </a>
          ))}
        </div>
      )}
    </div>
  );
}
