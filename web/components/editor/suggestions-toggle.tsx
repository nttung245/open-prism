"use client";

import { LoaderCircle, TriangleAlert, WandSparkles } from "lucide-react";

/** Switches ghost text suggestions on and off, and shows when they load or fail. */
export function SuggestionsToggle({
  on,
  loading,
  error,
  onToggle,
}: {
  on: boolean;
  loading: boolean;
  /** Why requests stopped; clicking retries. */
  error: string | null;
  onToggle: () => void;
}) {
  const label = error
    ? `Suggestions paused: ${error} Click to try again.`
    : on
      ? "Suggestions on: pause typing for a suggestion, Tab accepts, Esc dismisses, Alt+\\ asks now. Click to turn off."
      : "Suggestions off. Click to turn on.";
  return (
    <button
      type="button"
      onClick={onToggle}
      aria-pressed={on}
      aria-label={label}
      title={label}
      className={`inline-flex size-7 items-center justify-center rounded-md transition-colors hover:bg-zinc-200/60 focus-visible:outline-2 focus-visible:outline-accent dark:hover:bg-zinc-800/60 ${
        on ? "text-accent" : "text-muted opacity-60"
      }`}
    >
      {error ? (
        <TriangleAlert className="size-3.5 text-danger" />
      ) : loading ? (
        <LoaderCircle className="size-3.5 animate-spin" />
      ) : (
        <WandSparkles className="size-3.5" />
      )}
    </button>
  );
}
