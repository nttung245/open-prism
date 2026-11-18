/** Calls to the version history API of a project. */
import { apiJson } from "./client";
import type { ChangesResponse } from "./types/ChangesResponse";
import type { FileDiff } from "./types/FileDiff";
import type { HistoryPage } from "./types/HistoryPage";
import type { RestoreResult } from "./types/RestoreResult";
import type { Snapshot } from "./types/Snapshot";

/** What a snapshot is compared with: its parent, the current files, or another `rev`. */
export type Against = "parent" | "current" | (string & {});

function query(params: Record<string, string | number | null | undefined>) {
  const search = new URLSearchParams();
  for (const [key, value] of Object.entries(params)) {
    if (value !== null && value !== undefined && value !== "") {
      search.set(key, String(value));
    }
  }
  const text = search.toString();
  return text ? `?${text}` : "";
}

/** Query string of a timeline page. */
export function pageQuery(limit: number, before?: string | null): string {
  return query({ limit, before });
}

/** Query string of a comparison; `parent` is the server's default and is left out. */
export function diffQuery(against: Against, path?: string | null): string {
  return query({ against: against === "parent" ? null : against, path });
}

const base = (projectId: string) =>
  `/api/projects/${projectId}/history` as const;

export const historyApi = {
  page(projectId: string, limit: number, before?: string | null) {
    return apiJson<HistoryPage>(
      `${base(projectId)}${pageQuery(limit, before)}`,
    );
  },
  save(projectId: string, label: string) {
    return apiJson<Snapshot>(base(projectId), {
      method: "POST",
      json: { label },
    });
  },
  setLabel(projectId: string, rev: string, label: string | null) {
    return apiJson<Snapshot>(`${base(projectId)}/${rev}`, {
      method: "PATCH",
      json: { label },
    });
  },
  changes(projectId: string, rev: string, against: Against) {
    return apiJson<ChangesResponse>(
      `${base(projectId)}/${rev}/diff${diffQuery(against)}`,
    );
  },
  fileDiff(projectId: string, rev: string, against: Against, path: string) {
    return apiJson<FileDiff>(
      `${base(projectId)}/${rev}/diff${diffQuery(against, path)}`,
    );
  },
  /** Restores `paths`, or the whole project when `paths` is omitted. */
  restore(projectId: string, rev: string, paths?: string[]) {
    return apiJson<RestoreResult>(`${base(projectId)}/${rev}/restore`, {
      method: "POST",
      json: paths ? { paths } : {},
    });
  },
};
