"use client";

import { CircleAlert, CircleX, Info, Sparkles } from "lucide-react";
import type { CompileResult } from "@/lib/api/types/CompileResult";
import type { Diagnostic } from "@/lib/api/types/Diagnostic";

const ICONS = {
  error: <CircleX className="mt-0.5 size-4 shrink-0 text-danger" />,
  warning: <CircleAlert className="mt-0.5 size-4 shrink-0 text-warning" />,
  info: <Info className="mt-0.5 size-4 shrink-0 text-muted" />,
};

/** Diagnostics in TeX Live's own files carry absolute paths and cannot be opened. */
export function isProjectFile(file: string | null): file is string {
  return file !== null && !file.startsWith("/");
}

export function countBySeverity(diagnostics: Diagnostic[]) {
  const counts = { error: 0, warning: 0, info: 0 };
  for (const diagnostic of diagnostics) counts[diagnostic.severity] += 1;
  return counts;
}

/** Errors, warnings and typesetting notes of the last compilation. */
export function ProblemsPanel({
  result,
  logUrl,
  onSelect,
  onFix,
}: {
  result: CompileResult | null;
  logUrl: string | null;
  onSelect: (diagnostic: Diagnostic) => void;
  /** Asks the assistant to fix the errors. */
  onFix?: () => void;
}) {
  const diagnostics = result?.diagnostics ?? [];
  const errors = diagnostics.filter((d) => d.severity === "error").length;
  return (
    <div className="flex h-full min-h-0 flex-col">
      <div className="flex h-8 shrink-0 items-center gap-3 border-b border-border px-3 text-xs text-muted">
        <span className="font-medium uppercase tracking-wide">Problems</span>
        <span className="flex-1" />
        {onFix && errors > 0 && (
          <button
            type="button"
            onClick={onFix}
            className="inline-flex items-center gap-1 rounded px-1.5 py-0.5 text-accent hover:bg-accent-soft"
          >
            <Sparkles className="size-3" /> Fix with AI
          </button>
        )}
        {logUrl && (
          <a
            href={logUrl}
            target="_blank"
            rel="noreferrer"
            className="hover:text-foreground hover:underline"
          >
            Raw log
          </a>
        )}
      </div>
      <div className="min-h-0 flex-1 overflow-auto">
        {result?.message && (
          <pre className="whitespace-pre-wrap border-b border-border px-3 py-2 font-mono text-xs text-danger">
            {result.message}
          </pre>
        )}
        {diagnostics.length === 0 ? (
          <p className="px-3 py-2 text-xs text-muted">
            {result ? "No problems reported." : "Not compiled yet."}
          </p>
        ) : (
          <ul>
            {diagnostics.map((diagnostic, index) => (
              <li
                // biome-ignore lint/suspicious/noArrayIndexKey: diagnostics have no identity.
                key={index}
              >
                <button
                  type="button"
                  onClick={() => onSelect(diagnostic)}
                  disabled={!isProjectFile(diagnostic.file)}
                  className="flex w-full gap-2 px-3 py-1.5 text-left text-xs hover:bg-zinc-200/50 disabled:cursor-default dark:hover:bg-zinc-800/50"
                >
                  {ICONS[diagnostic.severity]}
                  <span className="min-w-0 flex-1">
                    <span className="block">{diagnostic.message}</span>
                    {diagnostic.details && (
                      <span className="block truncate font-mono text-muted">
                        {diagnostic.details.split("\n")[0]}
                      </span>
                    )}
                  </span>
                  {diagnostic.file && (
                    <span className="shrink-0 font-mono text-muted">
                      {diagnostic.file}
                      {diagnostic.line !== null && `:${diagnostic.line}`}
                    </span>
                  )}
                </button>
              </li>
            ))}
          </ul>
        )}
      </div>
    </div>
  );
}
