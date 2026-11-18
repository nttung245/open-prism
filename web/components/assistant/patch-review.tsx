"use client";

import { FilePlus2, FileText } from "lucide-react";
import { wordDiff } from "@/lib/ai/word-diff";
import type { Patch } from "@/lib/api/types/Patch";

/** Shows one proposed edit with a checkbox per change. */
export function PatchReview({
  patch,
  accepted,
  onChange,
  onOpen,
}: {
  patch: Patch;
  accepted: boolean[];
  onChange: (accepted: boolean[]) => void;
  onOpen: (file: string) => void;
}) {
  const all = accepted.every(Boolean);
  return (
    <div className="rounded-lg border border-border bg-background">
      <div className="flex items-center gap-2 border-b border-border px-3 py-2 text-xs">
        {patch.createsFile ? (
          <FilePlus2 className="size-3.5 text-accent" />
        ) : (
          <FileText className="size-3.5 text-muted" />
        )}
        <button
          type="button"
          onClick={() => onOpen(patch.file)}
          className="min-w-0 truncate font-mono hover:underline"
          title="Open the file"
        >
          {patch.file}
        </button>
        {patch.createsFile && <span className="text-muted">(new file)</span>}
        <span className="flex-1" />
        {patch.hunks.length > 1 && (
          <label className="flex items-center gap-1 text-muted">
            <input
              type="checkbox"
              checked={all}
              onChange={() => onChange(patch.hunks.map(() => !all))}
              className="accent-accent"
            />
            all
          </label>
        )}
      </div>
      {patch.summary && (
        <p className="px-3 pt-2 text-xs text-muted">{patch.summary}</p>
      )}
      <ol className="flex flex-col gap-2 p-2">
        {patch.hunks.map((hunk, index) => (
          // biome-ignore lint/suspicious/noArrayIndexKey: hunks are positional.
          <li key={index} className="flex gap-2">
            <input
              type="checkbox"
              aria-label={`Apply change ${index + 1}`}
              checked={accepted[index] ?? false}
              onChange={() =>
                onChange(
                  accepted.map((value, i) => (i === index ? !value : value)),
                )
              }
              className="mt-1 accent-accent"
            />
            <pre className="min-w-0 flex-1 overflow-x-auto whitespace-pre-wrap break-words rounded-md bg-panel p-2 font-mono text-xs leading-relaxed">
              {wordDiff(hunk.old, hunk.new).map((segment, i) =>
                segment.kind === "same" ? (
                  // biome-ignore lint/suspicious/noArrayIndexKey: static diff output.
                  <span key={i}>{segment.text}</span>
                ) : segment.kind === "removed" ? (
                  <del
                    // biome-ignore lint/suspicious/noArrayIndexKey: static diff output.
                    key={i}
                    className="bg-red-200/70 text-red-900 dark:bg-red-950 dark:text-red-200"
                  >
                    {segment.text}
                  </del>
                ) : (
                  <ins
                    // biome-ignore lint/suspicious/noArrayIndexKey: static diff output.
                    key={i}
                    className="bg-emerald-200/70 text-emerald-900 no-underline dark:bg-emerald-950 dark:text-emerald-200"
                  >
                    {segment.text}
                  </ins>
                ),
              )}
            </pre>
          </li>
        ))}
      </ol>
    </div>
  );
}
