"use client";

import {
  useInfiniteQuery,
  useQuery,
  useQueryClient,
} from "@tanstack/react-query";
import {
  Bookmark,
  BookmarkPlus,
  ChevronDown,
  ChevronRight,
  CircleCheck,
  FilePlus2,
  History,
  LoaderCircle,
  RotateCcw,
  Sparkles,
  Tag,
  Undo2,
} from "lucide-react";
import { type ReactNode, useState } from "react";
import { Button, IconButton } from "@/components/ui/button";
import { errorMessage } from "@/lib/api/client";
import { type Against, historyApi } from "@/lib/api/history";
import type { ChangedFile } from "@/lib/api/types/ChangedFile";
import type { Snapshot } from "@/lib/api/types/Snapshot";
import type { SnapshotKind } from "@/lib/api/types/SnapshotKind";
import { diffRows } from "@/lib/history/diff-rows";
import type { DocumentStore } from "@/lib/workspace/documents";

const PAGE_SIZE = 50;
const MAX_LABEL = 100;

const KINDS: Record<SnapshotKind, { label: string; icon: ReactNode }> = {
  created: {
    label: "Created",
    icon: <FilePlus2 className="size-3.5 text-muted" />,
  },
  beforeAiEdit: {
    label: "Before AI edit",
    icon: <Sparkles className="size-3.5 text-muted" />,
  },
  aiEdit: {
    label: "AI edit",
    icon: <Sparkles className="size-3.5 text-accent" />,
  },
  compiled: {
    label: "Compiled",
    icon: <CircleCheck className="size-3.5 text-muted" />,
  },
  manual: {
    label: "Saved",
    icon: <Bookmark className="size-3.5 text-accent" />,
  },
  beforeRestore: {
    label: "Before restore",
    icon: <Undo2 className="size-3.5 text-muted" />,
  },
  restored: {
    label: "Restored",
    icon: <RotateCcw className="size-3.5 text-muted" />,
  },
};

const UNITS: Array<[Intl.RelativeTimeFormatUnit, number]> = [
  ["year", 365 * 24 * 3600],
  ["month", 30 * 24 * 3600],
  ["day", 24 * 3600],
  ["hour", 3600],
  ["minute", 60],
];

function timeAgo(ms: number, now = Date.now()): string {
  const seconds = Math.round((ms - now) / 1000);
  const format = new Intl.RelativeTimeFormat("en", { numeric: "auto" });
  for (const [unit, size] of UNITS) {
    if (Math.abs(seconds) >= size) {
      return format.format(Math.round(seconds / size), unit);
    }
  }
  return "just now";
}

/** Asks for a label; `undefined` when cancelled, `null` to remove it. */
function askLabel(message: string, current: string | null) {
  const answer = window.prompt(message, current ?? "");
  if (answer === null) return undefined;
  const label = answer.trim();
  if (label.length > MAX_LABEL) {
    window.alert(`A label has at most ${MAX_LABEL} characters.`);
    return undefined;
  }
  return label || null;
}

/** The project's timeline: saved versions, what changed, and restoring them. */
export function HistoryPanel({
  projectId,
  store,
  onOpenFile,
  onNotice,
  onRestored,
}: {
  projectId: string;
  store: DocumentStore;
  onOpenFile: (path: string) => void;
  onNotice: (message: string) => void;
  /** Files were restored (e.g. to compile again). */
  onRestored?: () => void;
}) {
  const queryClient = useQueryClient();
  const [selected, setSelected] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const pages = useInfiniteQuery({
    queryKey: ["history", projectId, "timeline"],
    queryFn: ({ pageParam }) =>
      historyApi.page(projectId, PAGE_SIZE, pageParam),
    initialPageParam: null as string | null,
    getNextPageParam: (last) => last.nextBefore,
  });
  const snapshots = pages.data?.pages.flatMap((page) => page.snapshots) ?? [];

  const run = async (action: () => Promise<void>) => {
    setBusy(true);
    try {
      await action();
    } catch (error) {
      onNotice(errorMessage(error));
    } finally {
      setBusy(false);
    }
  };

  const saveVersion = () => {
    const label = askLabel("Name this version", null);
    if (!label) return;
    void run(async () => {
      await store.flush();
      await historyApi.save(projectId, label);
      await queryClient.invalidateQueries({ queryKey: ["history", projectId] });
    });
  };

  const setLabel = (snapshot: Snapshot) => {
    const label = askLabel(
      "Label of this version (empty to remove it)",
      snapshot.label,
    );
    if (label === undefined) return;
    void run(async () => {
      await historyApi.setLabel(projectId, snapshot.rev, label);
      await queryClient.invalidateQueries({ queryKey: ["history", projectId] });
    });
  };

  const restore = (snapshot: Snapshot, file?: ChangedFile) => {
    const name = snapshot.label
      ? `“${snapshot.label}”`
      : `the version of ${new Date(snapshot.createdAt).toLocaleString()}`;
    if (!file) {
      const ok = window.confirm(
        `Restore the whole project to ${name}?\n\nThe current files are saved in the history first, so this can be undone.`,
      );
      if (!ok) return;
    } else if (file.newSize === null) {
      const ok = window.confirm(
        `${file.path} is not in ${name}: restoring it deletes the file.\n\nThe current files are saved in the history first.`,
      );
      if (!ok) return;
    }
    void run(async () => {
      // Unsaved edits are saved first so the snapshot taken before restoring has them.
      await store.flush();
      const result = await historyApi.restore(
        projectId,
        snapshot.rev,
        file ? [file.path] : undefined,
      );
      await store.refresh(result.changed);
      await queryClient.invalidateQueries({ queryKey: ["tree", projectId] });
      await queryClient.invalidateQueries({ queryKey: ["history", projectId] });
      const count = result.changed.length;
      const files = `${count} file${count === 1 ? "" : "s"} changed`;
      onNotice(
        count === 0
          ? "Nothing to restore: the files already match this version."
          : file
            ? `Restored ${file.path} from ${name}.`
            : `Restored the project from ${name} (${files}).`,
      );
      if (count > 0) onRestored?.();
    });
  };

  return (
    <div className="flex h-full min-h-0 flex-col bg-panel">
      <div className="flex h-9 shrink-0 items-center gap-1 border-b border-border bg-background px-2">
        <History className="size-4 text-accent" />
        <span className="flex-1 text-xs font-medium">History</span>
        {busy && <LoaderCircle className="size-3.5 animate-spin text-muted" />}
        <Button
          variant="ghost"
          className="h-7 px-2 text-xs"
          onClick={saveVersion}
          disabled={busy}
          title="Save the current files as a named version"
        >
          <BookmarkPlus className="size-3.5" /> Save version…
        </Button>
      </div>
      <div className="min-h-0 flex-1 overflow-y-auto">
        {pages.error ? (
          <p className="p-3 text-xs text-danger">{errorMessage(pages.error)}</p>
        ) : pages.isPending ? (
          <p className="p-3 text-xs text-muted">Loading…</p>
        ) : snapshots.length === 0 ? (
          <div className="flex flex-col items-start gap-2 p-3 text-xs text-muted">
            <p>
              No versions yet. A version is saved automatically when the project
              compiles, when you accept an AI edit and when you restore one; you
              can also save one yourself.
            </p>
            <Button className="text-xs" onClick={saveVersion} disabled={busy}>
              <BookmarkPlus className="size-3.5" /> Save version…
            </Button>
          </div>
        ) : (
          <ol className="flex flex-col">
            {snapshots.map((snapshot) => (
              <TimelineItem
                key={snapshot.rev}
                projectId={projectId}
                snapshot={snapshot}
                open={selected === snapshot.rev}
                busy={busy}
                onToggle={() =>
                  setSelected(selected === snapshot.rev ? null : snapshot.rev)
                }
                onLabel={() => setLabel(snapshot)}
                onRestore={(file) => restore(snapshot, file)}
                onOpenFile={onOpenFile}
              />
            ))}
          </ol>
        )}
        {pages.hasNextPage && (
          <div className="p-2">
            <Button
              className="w-full text-xs"
              onClick={() => void pages.fetchNextPage()}
              disabled={pages.isFetchingNextPage}
            >
              {pages.isFetchingNextPage ? "Loading…" : "Load more"}
            </Button>
          </div>
        )}
      </div>
    </div>
  );
}

function Counts({ snapshot }: { snapshot: Snapshot }) {
  const parts = [
    snapshot.added > 0 && (
      <span key="a" className="text-emerald-700 dark:text-emerald-400">
        +{snapshot.added}
      </span>
    ),
    snapshot.modified > 0 && (
      <span key="m" className="text-amber-700 dark:text-amber-400">
        ~{snapshot.modified}
      </span>
    ),
    snapshot.deleted > 0 && (
      <span key="d" className="text-red-700 dark:text-red-400">
        −{snapshot.deleted}
      </span>
    ),
  ].filter(Boolean);
  if (parts.length === 0) return null;
  return (
    <span
      className="flex gap-1 font-mono text-[11px]"
      title="Files added, modified and deleted"
    >
      {parts}
    </span>
  );
}

function TimelineItem({
  projectId,
  snapshot,
  open,
  busy,
  onToggle,
  onLabel,
  onRestore,
  onOpenFile,
}: {
  projectId: string;
  snapshot: Snapshot;
  open: boolean;
  busy: boolean;
  onToggle: () => void;
  onLabel: () => void;
  onRestore: (file?: ChangedFile) => void;
  onOpenFile: (path: string) => void;
}) {
  const kind = KINDS[snapshot.kind];
  // A labelled manual version says it all with its label.
  const labelOnly = snapshot.kind === "manual" && snapshot.label !== null;
  return (
    <li className="border-b border-border">
      <button
        type="button"
        onClick={onToggle}
        aria-expanded={open}
        className="flex w-full items-start gap-2 px-3 py-2 text-left hover:bg-background"
      >
        <span className="mt-0.5" title={kind.label}>
          {kind.icon}
        </span>
        <span className="flex min-w-0 flex-1 flex-col gap-0.5">
          <span className="flex items-center gap-1.5">
            {snapshot.label && (
              <span
                className={`${labelOnly ? "max-w-full" : "max-w-[60%]"} shrink-0 truncate rounded bg-accent-soft px-1.5 py-px text-[11px] font-medium text-foreground`}
                title={snapshot.label}
              >
                {snapshot.label}
              </span>
            )}
            {!labelOnly && (
              <span className="truncate text-xs" title={snapshot.summary}>
                {snapshot.summary}
              </span>
            )}
          </span>
          <span className="flex flex-wrap items-center gap-x-2 text-[11px] whitespace-nowrap text-muted">
            <time
              dateTime={new Date(snapshot.createdAt).toISOString()}
              title={new Date(snapshot.createdAt).toLocaleString()}
            >
              {timeAgo(snapshot.createdAt)}
            </time>
            <span>{kind.label}</span>
            <Counts snapshot={snapshot} />
          </span>
        </span>
        {open ? (
          <ChevronDown className="mt-0.5 size-3.5 shrink-0 text-muted" />
        ) : (
          <ChevronRight className="mt-0.5 size-3.5 shrink-0 text-muted" />
        )}
      </button>
      {open && (
        <SnapshotDetails
          projectId={projectId}
          snapshot={snapshot}
          busy={busy}
          onLabel={onLabel}
          onRestore={onRestore}
          onOpenFile={onOpenFile}
        />
      )}
    </li>
  );
}

const STATUS = {
  added: { letter: "A", className: "text-emerald-700 dark:text-emerald-400" },
  modified: { letter: "M", className: "text-amber-700 dark:text-amber-400" },
  deleted: { letter: "D", className: "text-red-700 dark:text-red-400" },
} as const;

function SnapshotDetails({
  projectId,
  snapshot,
  busy,
  onLabel,
  onRestore,
  onOpenFile,
}: {
  projectId: string;
  snapshot: Snapshot;
  busy: boolean;
  onLabel: () => void;
  onRestore: (file?: ChangedFile) => void;
  onOpenFile: (path: string) => void;
}) {
  const [against, setAgainst] = useState<Against>("parent");
  const [shown, setShown] = useState<string | null>(null);
  const changes = useQuery({
    queryKey: ["history", projectId, "changes", snapshot.rev, against],
    queryFn: () => historyApi.changes(projectId, snapshot.rev, against),
    staleTime: against === "current" ? 0 : Number.POSITIVE_INFINITY,
  });
  const files = changes.data?.files ?? [];
  return (
    <div className="flex flex-col gap-2 bg-background px-3 pt-1 pb-3">
      <div className="flex flex-wrap items-center gap-1.5">
        <Button
          className="h-7 px-2 text-xs"
          onClick={() => onRestore()}
          disabled={busy}
          title="Make every file match this version"
        >
          <RotateCcw className="size-3.5" /> Restore project…
        </Button>
        <Button
          variant="ghost"
          className="h-7 px-2 text-xs"
          onClick={onLabel}
          disabled={busy}
        >
          <Tag className="size-3.5" /> {snapshot.label ? "Rename…" : "Label…"}
        </Button>
        <label className="ml-auto flex items-center gap-1 text-[11px] text-muted">
          <input
            type="checkbox"
            checked={against === "current"}
            onChange={(event) => {
              setAgainst(event.target.checked ? "current" : "parent");
              setShown(null);
            }}
            className="accent-accent"
          />
          Compare with current files
        </label>
      </div>
      <p className="text-[11px] text-muted">
        {against === "current"
          ? "What restoring this version would change in the current files:"
          : "Changes in this version:"}
      </p>
      {changes.error ? (
        <p className="text-xs text-danger">{errorMessage(changes.error)}</p>
      ) : changes.isPending ? (
        <p className="text-xs text-muted">Loading…</p>
      ) : files.length === 0 ? (
        <p className="text-xs text-muted">
          {against === "current"
            ? "The current files match this version."
            : "No file changed."}
        </p>
      ) : (
        <ul className="flex flex-col rounded-md border border-border">
          {files.map((file) => (
            <li
              key={file.path}
              className="border-b border-border last:border-b-0"
            >
              <div className="flex items-center gap-1.5 px-2 py-1 text-xs">
                <button
                  type="button"
                  onClick={() =>
                    setShown(shown === file.path ? null : file.path)
                  }
                  aria-expanded={shown === file.path}
                  className="flex min-w-0 flex-1 items-center gap-1.5 text-left"
                  title="Show the changes"
                >
                  <span
                    className={`w-3 shrink-0 font-mono font-semibold ${STATUS[file.status].className}`}
                  >
                    {STATUS[file.status].letter}
                  </span>
                  <span className="truncate font-mono">{file.path}</span>
                  {file.additions !== null && file.deletions !== null && (
                    <span className="shrink-0 font-mono text-[11px] text-muted">
                      +{file.additions} −{file.deletions}
                    </span>
                  )}
                  {file.binary && (
                    <span className="shrink-0 text-[11px] text-muted">
                      binary
                    </span>
                  )}
                </button>
                <IconButton
                  label={`Restore ${file.path} from this version`}
                  className="size-6"
                  onClick={() => onRestore(file)}
                  disabled={busy}
                >
                  <RotateCcw className="size-3.5" />
                </IconButton>
              </div>
              {shown === file.path && (
                <FileDiffView
                  projectId={projectId}
                  rev={snapshot.rev}
                  against={against}
                  file={file}
                  onOpenFile={onOpenFile}
                />
              )}
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

function formatSize(bytes: number | null) {
  if (bytes === null) return "none";
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
}

const ROW_STYLE = {
  context: "",
  removed: "bg-red-100/70 dark:bg-red-950/50",
  added: "bg-emerald-100/70 dark:bg-emerald-950/50",
} as const;

const WORD_STYLE = {
  same: "",
  removed: "bg-red-300/70 dark:bg-red-800/70",
  added: "bg-emerald-300/70 dark:bg-emerald-800/70",
} as const;

function FileDiffView({
  projectId,
  rev,
  against,
  file,
  onOpenFile,
}: {
  projectId: string;
  rev: string;
  against: Against;
  file: ChangedFile;
  onOpenFile: (path: string) => void;
}) {
  const diff = useQuery({
    queryKey: ["history", projectId, "diff", rev, against, file.path],
    queryFn: () => historyApi.fileDiff(projectId, rev, against, file.path),
    staleTime: against === "current" ? 0 : Number.POSITIVE_INFINITY,
  });
  if (diff.error) {
    return (
      <p className="px-2 pb-2 text-xs text-danger">
        {errorMessage(diff.error)}
      </p>
    );
  }
  if (!diff.data) {
    return <p className="px-2 pb-2 text-xs text-muted">Loading…</p>;
  }
  const data = diff.data;
  if (data.binary || data.tooLarge) {
    return (
      <p className="px-2 pb-2 text-xs text-muted">
        {data.binary ? "Binary file" : "File too large to compare"}:{" "}
        {formatSize(data.oldSize)} → {formatSize(data.newSize)}
      </p>
    );
  }
  return (
    <div className="border-t border-border">
      {data.hunks.map((hunk) => (
        <div
          key={`${hunk.oldStart}-${hunk.newStart}`}
          className="border-b border-border last:border-b-0"
        >
          <button
            type="button"
            onClick={() => onOpenFile(file.path)}
            className="w-full bg-panel px-2 py-0.5 text-left font-mono text-[11px] text-muted hover:text-foreground"
            title="Open the file"
          >
            @@ −{hunk.oldStart},{hunk.oldLines} +{hunk.newStart},{hunk.newLines}{" "}
            @@
          </button>
          <table className="w-full table-fixed border-collapse font-mono text-[11px] leading-snug">
            <tbody>
              {diffRows(hunk).map((row, index) => (
                <tr
                  // biome-ignore lint/suspicious/noArrayIndexKey: static diff output.
                  key={index}
                  className={ROW_STYLE[row.kind]}
                >
                  <td className="w-8 select-none pr-1 text-right align-top text-muted">
                    {row.oldNumber ?? ""}
                  </td>
                  <td className="w-8 select-none pr-1 text-right align-top text-muted">
                    {row.newNumber ?? ""}
                  </td>
                  <td className="w-3 select-none align-top text-muted">
                    {row.kind === "added"
                      ? "+"
                      : row.kind === "removed"
                        ? "−"
                        : ""}
                  </td>
                  <td className="pr-2 whitespace-pre-wrap [overflow-wrap:anywhere]">
                    {row.segments.map((segment, n) => (
                      <span
                        // biome-ignore lint/suspicious/noArrayIndexKey: static diff output.
                        key={n}
                        className={WORD_STYLE[segment.kind]}
                      >
                        {segment.text}
                      </span>
                    ))}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      ))}
    </div>
  );
}
