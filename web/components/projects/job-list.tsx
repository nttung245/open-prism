"use client";

import {
  type QueryClient,
  useMutation,
  useQuery,
  useQueryClient,
} from "@tanstack/react-query";
import { ExternalLink, RotateCcw, Square, X } from "lucide-react";
import Link from "next/link";
import { useEffect, useRef } from "react";
import { Button } from "@/components/ui/button";
import { apiFetch, apiJson, errorMessage } from "@/lib/api/client";
import { useAppEvents } from "@/lib/api/events";
import type { JobState } from "@/lib/api/types/JobState";
import type { JobView } from "@/lib/api/types/JobView";
import {
  ACTIVE_POLL_MS,
  applyEvent,
  hasActiveJobs,
  isFinished,
  JOBS_KEY,
  mergeFetched,
  progress,
  sortJobs,
  upsert,
} from "@/lib/jobs";

const STATE_LABEL: Record<JobState, string> = {
  queued: "Waiting",
  running: "Running",
  succeeded: "Done",
  failed: "Failed",
  cancelled: "Cancelled",
};

const STATE_CLASS: Record<JobState, string> = {
  queued: "text-muted",
  running: "text-accent",
  succeeded: "text-emerald-700 dark:text-emerald-400",
  failed: "text-danger",
  cancelled: "text-muted",
};

/** Puts a job the page just started or changed into the list, before its event arrives. */
export function showJob(queryClient: QueryClient, job: JobView) {
  queryClient.setQueryData<JobView[]>(JOBS_KEY, (jobs) =>
    upsert(jobs ?? [], job),
  );
}

/**
 * Queued, running and recently finished background jobs, kept up to date by `/api/events`
 * without reloading the page, and fetched again every few seconds while one waits or runs in
 * case the events do not get through. `onProjectsChanged` runs when a job may have made a
 * project.
 */
export function JobList({
  initial,
  onProjectsChanged,
}: {
  initial: JobView[];
  onProjectsChanged: () => void;
}) {
  const queryClient = useQueryClient();
  const jobs = useQuery({
    queryKey: JOBS_KEY,
    queryFn: async () =>
      mergeFetched(
        queryClient.getQueryData<JobView[]>(JOBS_KEY) ?? [],
        await apiJson<JobView[]>("/api/jobs"),
      ),
    initialData: () => sortJobs(initial),
    refetchInterval: (query) =>
      hasActiveJobs(query.state.data) ? ACTIVE_POLL_MS : false,
  });
  // A job that finished between two fetches may have made a project.
  const finishedCount = jobs.data.filter(
    (job) => job.state === "succeeded",
  ).length;
  const seenFinished = useRef(finishedCount);
  useEffect(() => {
    if (finishedCount > seenFinished.current) onProjectsChanged();
    seenFinished.current = finishedCount;
  }, [finishedCount, onProjectsChanged]);

  useAppEvents((event) => {
    if (
      (event.type === "jobChanged" && event.job.state === "succeeded") ||
      // Someone added this person to a project, or removed them from one.
      event.type === "projectsChanged"
    ) {
      onProjectsChanged();
    }
    const next = applyEvent(
      queryClient.getQueryData<JobView[]>(JOBS_KEY) ?? [],
      event,
    );
    if (next === "resync") {
      void queryClient.invalidateQueries({ queryKey: JOBS_KEY });
      onProjectsChanged();
    } else {
      queryClient.setQueryData(JOBS_KEY, next);
    }
  });

  if (jobs.data.length === 0) return null;
  return (
    <section aria-label="Background jobs">
      <ul className="divide-y divide-border rounded-lg border border-border">
        {jobs.data.map((job) => (
          <JobRow key={job.id} job={job} />
        ))}
      </ul>
    </section>
  );
}

function JobRow({ job }: { job: JobView }) {
  const queryClient = useQueryClient();
  const act = useMutation({
    mutationFn: async (action: "cancel" | "retry" | "dismiss") => {
      if (action === "dismiss") {
        await apiFetch(`/api/jobs/${job.id}`, { method: "DELETE" });
        return null;
      }
      return apiJson<JobView>(`/api/jobs/${job.id}/${action}`, {
        method: "POST",
      });
    },
    onSuccess: (changed) => {
      if (changed) {
        showJob(queryClient, changed);
      } else {
        queryClient.setQueryData<JobView[]>(JOBS_KEY, (jobs) =>
          (jobs ?? []).filter((other) => other.id !== job.id),
        );
      }
    },
  });

  const finished = isFinished(job.state);
  const share = progress(job);
  const percent = share === null ? null : Math.round(share * 100);
  const unit = job.kind === "pdfImport" ? " pages" : "";

  return (
    <li className="flex flex-col gap-1.5 px-4 py-3" data-job-state={job.state}>
      <div className="flex items-center gap-3">
        <span className="min-w-0 flex-1 truncate font-medium">{job.title}</span>
        <span
          className={`shrink-0 text-xs font-medium ${STATE_CLASS[job.state]}`}
        >
          {STATE_LABEL[job.state]}
          {job.stepsTotal !== null &&
            job.state !== "succeeded" &&
            ` · ${job.stepsDone}/${job.stepsTotal}${unit}`}
        </span>
        <div className="flex shrink-0 gap-1">
          {job.state === "succeeded" && job.projectId && (
            <Link
              href={`/project/${job.projectId}`}
              className="inline-flex items-center gap-1.5 rounded-md border border-border bg-background px-2.5 py-1.5 text-sm font-medium hover:bg-panel focus-visible:outline-2 focus-visible:outline-accent"
            >
              <ExternalLink className="size-4" /> Open project
            </Link>
          )}
          {job.state === "failed" && (
            <Button
              disabled={act.isPending}
              onClick={() => act.mutate("retry")}
            >
              <RotateCcw className="size-4" /> Retry
            </Button>
          )}
          {!finished && (
            <Button
              variant="ghost"
              disabled={act.isPending}
              onClick={() => act.mutate("cancel")}
            >
              <Square className="size-3.5" /> Cancel
            </Button>
          )}
          {finished && (
            <Button
              variant="ghost"
              disabled={act.isPending}
              onClick={() => act.mutate("dismiss")}
              title="Remove from the list"
            >
              <X className="size-4" /> Dismiss
            </Button>
          )}
        </div>
      </div>
      {!finished && (
        <div className="flex items-center gap-3">
          <div
            role="progressbar"
            aria-label={`Progress of ${job.title}`}
            aria-valuemin={0}
            aria-valuemax={100}
            aria-valuenow={percent ?? undefined}
            className="relative h-1 flex-1 overflow-hidden rounded-full bg-border"
          >
            {percent === null ? (
              <div className="job-progress-indeterminate absolute inset-y-0 left-0 w-1/3 rounded-full bg-accent" />
            ) : (
              <div
                className="h-full rounded-full bg-accent transition-[width] duration-700 ease-out"
                style={{ width: `${percent}%` }}
              />
            )}
          </div>
          <span className="w-9 shrink-0 text-right text-xs text-muted tabular-nums">
            {percent === null ? "" : `${percent}%`}
          </span>
        </div>
      )}
      {job.message && !finished && (
        <p className="text-xs text-muted">{job.message}</p>
      )}
      {job.error && (
        <p className="text-xs break-words text-danger">{job.error}</p>
      )}
      {act.error && (
        <p role="alert" className="text-xs text-danger">
          {errorMessage(act.error)}
        </p>
      )}
    </li>
  );
}
