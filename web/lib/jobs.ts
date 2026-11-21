/**
 * The background jobs a page shows: the list from `GET /api/jobs`, kept up to date by the
 * events of `/api/events`.
 */
import type { AppEvent } from "./api/types/AppEvent";
import type { JobState } from "./api/types/JobState";
import type { JobView } from "./api/types/JobView";

export const JOBS_KEY = ["jobs"] as const;

export function isFinished(state: JobState): boolean {
  return state === "succeeded" || state === "failed" || state === "cancelled";
}

/** The list after `event`, or `"resync"` when the list must be fetched again. */
export function applyEvent(
  jobs: JobView[],
  event: AppEvent,
): JobView[] | "resync" {
  switch (event.type) {
    case "jobChanged":
      return upsert(jobs, event.job);
    case "jobRemoved":
      return jobs.filter((job) => job.id !== event.id);
    case "resync":
      return "resync";
  }
}

/** Adds `job`, or puts it in place of the job with its id unless that one is newer. */
export function upsert(jobs: JobView[], job: JobView): JobView[] {
  const index = jobs.findIndex((other) => other.id === job.id);
  if (index === -1) return sortJobs([...jobs, job]);
  // A list fetched before this event may arrive after it: keep the newer view.
  if (jobs[index].updatedAt > job.updatedAt) return jobs;
  const next = [...jobs];
  next[index] = job;
  return sortJobs(next);
}

const RANK: Record<JobState, number> = {
  running: 0,
  queued: 1,
  succeeded: 2,
  failed: 2,
  cancelled: 2,
};

/** Running jobs, then queued ones in the order they will run, then finished ones, latest first. */
export function sortJobs(jobs: JobView[]): JobView[] {
  return [...jobs].sort(
    (a, b) =>
      RANK[a.state] - RANK[b.state] ||
      (isFinished(a.state)
        ? (b.finishedAt ?? b.updatedAt) - (a.finishedAt ?? a.updatedAt)
        : a.createdAt - b.createdAt) ||
      a.id.localeCompare(b.id),
  );
}

/** Progress from 0 to 1, or `null` while the number of steps is unknown. */
export function progress(job: JobView): number | null {
  if (job.state === "succeeded") return 1;
  if (job.stepsTotal === null || job.stepsTotal === 0) return null;
  return Math.min(job.stepsDone / job.stepsTotal, 1);
}
