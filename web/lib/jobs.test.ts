import { describe, expect, it } from "vitest";
import type { JobView } from "./api/types/JobView";
import { applyEvent, isFinished, progress, sortJobs, upsert } from "./jobs";

function job(id: string, patch: Partial<JobView> = {}): JobView {
  return {
    id,
    kind: "pdfImport",
    title: `Import ${id}.pdf`,
    state: "queued",
    stepsDone: 0,
    stepsTotal: null,
    message: null,
    error: null,
    projectId: null,
    createdAt: 1000,
    updatedAt: 1000,
    finishedAt: null,
    ...patch,
  };
}

const ids = (jobs: JobView[]) => jobs.map((one) => one.id);

describe("jobs", () => {
  it("adds a job it has not seen", () => {
    const list = applyEvent([job("a")], {
      type: "jobChanged",
      job: job("b", { createdAt: 2000, updatedAt: 2000 }),
    });
    expect(list !== "resync" && ids(list)).toEqual(["a", "b"]);
  });

  it("replaces a job with its newer view", () => {
    const list = applyEvent([job("a")], {
      type: "jobChanged",
      job: job("a", {
        state: "running",
        stepsDone: 1,
        stepsTotal: 3,
        updatedAt: 1500,
      }),
    });
    expect(list).toEqual([
      job("a", {
        state: "running",
        stepsDone: 1,
        stepsTotal: 3,
        updatedAt: 1500,
      }),
    ]);
  });

  it("keeps the newer view when an older one arrives late", () => {
    const newer = job("a", { state: "running", stepsDone: 2, updatedAt: 3000 });
    const list = [newer];
    expect(upsert(list, job("a", { updatedAt: 1000 }))).toBe(list);
    // Equal times: the event wins, as events come in order.
    const same = job("a", { state: "running", stepsDone: 3, updatedAt: 3000 });
    expect(upsert(list, same)).toEqual([same]);
  });

  it("removes a deleted job", () => {
    const list = applyEvent([job("a"), job("b", { createdAt: 2000 })], {
      type: "jobRemoved",
      id: "a",
    });
    expect(list !== "resync" && ids(list)).toEqual(["b"]);
    expect(applyEvent([job("b")], { type: "jobRemoved", id: "zz" })).toEqual([
      job("b"),
    ]);
  });

  it("asks for the list again on resync", () => {
    expect(applyEvent([job("a")], { type: "resync" })).toBe("resync");
  });

  it("orders running, then queued by age, then finished latest first", () => {
    const list = sortJobs([
      job("done-early", { state: "succeeded", finishedAt: 5000 }),
      job("queued-new", { createdAt: 3000 }),
      job("failed-late", { state: "failed", finishedAt: 9000 }),
      job("running", { state: "running", createdAt: 4000 }),
      job("queued-old", { createdAt: 2000 }),
      job("cancelled", { state: "cancelled", finishedAt: 7000 }),
    ]);
    expect(ids(list)).toEqual([
      "running",
      "queued-old",
      "queued-new",
      "failed-late",
      "cancelled",
      "done-early",
    ]);
  });

  it("tells finished states and progress", () => {
    expect(isFinished("running")).toBe(false);
    expect(isFinished("queued")).toBe(false);
    expect(isFinished("cancelled")).toBe(true);
    expect(progress(job("a"))).toBeNull();
    expect(progress(job("a", { stepsDone: 1, stepsTotal: 4 }))).toBe(0.25);
    expect(progress(job("a", { stepsTotal: 0 }))).toBeNull();
    expect(progress(job("a", { state: "succeeded", stepsTotal: null }))).toBe(
      1,
    );
  });
});
