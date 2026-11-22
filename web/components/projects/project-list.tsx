"use client";

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  FilePlus2,
  FileText,
  FileUp,
  LoaderCircle,
  Pencil,
  Trash2,
} from "lucide-react";
import Link from "next/link";
import { useRouter } from "next/navigation";
import { type SubmitEvent, useRef, useState } from "react";
import { Button, IconButton } from "@/components/ui/button";
import { DownloadMenu } from "@/components/ui/download-menu";
import { apiFetch, apiJson, errorMessage } from "@/lib/api/client";
import type { ImportResponse } from "@/lib/api/types/ImportResponse";
import type { JobView } from "@/lib/api/types/JobView";
import type { Project } from "@/lib/api/types/Project";
import { ImportPdfDialog } from "./import-pdf-dialog";
import { JobList, showJob } from "./job-list";
import { MembersButton } from "./members-dialog";

const PROJECTS_KEY = ["projects"] as const;

export function ProjectList({
  initial,
  initialJobs,
}: {
  initial: Project[];
  initialJobs: JobView[];
}) {
  const router = useRouter();
  const queryClient = useQueryClient();
  const projects = useQuery({
    queryKey: PROJECTS_KEY,
    queryFn: () => apiJson<Project[]>("/api/projects"),
    initialData: initial,
  });
  const [name, setName] = useState("");
  const [notice, setNotice] = useState<string | null>(null);
  const [importingPdf, setImportingPdf] = useState(false);
  const fileInput = useRef<HTMLInputElement>(null);

  const create = useMutation({
    mutationFn: (projectName: string) =>
      apiJson<Project>("/api/projects", {
        method: "POST",
        json: { name: projectName, template: "article" },
      }),
    onSuccess: (project) => router.push(`/project/${project.id}`),
  });

  const importZip = useMutation({
    mutationFn: async (file: File) => {
      const form = new FormData();
      form.append("file", file);
      return apiJson<ImportResponse>("/api/projects/import", {
        method: "POST",
        body: form,
      });
    },
    onSuccess: (result) => {
      if (result.skipped.length > 0) {
        setNotice(
          `Skipped ${result.skipped.length} archive entr${result.skipped.length === 1 ? "y" : "ies"}: ${result.skipped
            .slice(0, 5)
            .map((entry) => `${entry.name} (${entry.reason})`)
            .join(", ")}`,
        );
      }
      router.push(`/project/${result.project.id}`);
    },
  });

  const rename = useMutation({
    mutationFn: ({ id, newName }: { id: string; newName: string }) =>
      apiJson<Project>(`/api/projects/${id}`, {
        method: "PATCH",
        json: { name: newName },
      }),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: PROJECTS_KEY }),
  });

  const remove = useMutation({
    mutationFn: (id: string) =>
      apiFetch(`/api/projects/${id}`, { method: "DELETE" }),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: PROJECTS_KEY }),
  });

  const onCreate = (event: SubmitEvent<HTMLFormElement>) => {
    event.preventDefault();
    const trimmed = name.trim();
    if (trimmed) create.mutate(trimmed);
  };

  const error = create.error ?? importZip.error ?? rename.error ?? remove.error;
  const busy = create.isPending || importZip.isPending;

  return (
    <section className="flex flex-col gap-4">
      <form onSubmit={onCreate} className="flex flex-wrap gap-2">
        <input
          value={name}
          onChange={(event) => setName(event.target.value)}
          placeholder="New project name"
          aria-label="New project name"
          maxLength={200}
          className="min-w-0 flex-1 rounded-md border border-border bg-background px-3 py-1.5 text-sm outline-none focus:border-accent"
        />
        <Button type="submit" variant="primary" disabled={busy || !name.trim()}>
          {create.isPending ? (
            <LoaderCircle className="size-4 animate-spin" />
          ) : (
            <FilePlus2 className="size-4" />
          )}
          Create
        </Button>
        <Button
          disabled={busy}
          onClick={() => fileInput.current?.click()}
          title="Import a ZIP exported from Prism, Overleaf or arXiv"
        >
          {importZip.isPending ? (
            <LoaderCircle className="size-4 animate-spin" />
          ) : (
            <FileUp className="size-4" />
          )}
          Import ZIP
        </Button>
        <Button
          disabled={busy}
          onClick={() => setImportingPdf(true)}
          title="Turn a PDF into a LaTeX project with the Agent model"
        >
          <FileText className="size-4" />
          Import PDF
        </Button>
        <input
          ref={fileInput}
          type="file"
          accept=".zip,application/zip"
          className="hidden"
          onChange={(event) => {
            const file = event.target.files?.[0];
            event.target.value = "";
            if (file) importZip.mutate(file);
          }}
        />
      </form>

      {error && (
        <p role="alert" className="text-sm text-danger">
          {errorMessage(error)}
        </p>
      )}
      {notice && <p className="text-sm text-warning">{notice}</p>}

      <JobList
        initial={initialJobs}
        onProjectsChanged={() =>
          void queryClient.invalidateQueries({ queryKey: PROJECTS_KEY })
        }
      />
      <ImportPdfDialog
        open={importingPdf}
        onClose={() => setImportingPdf(false)}
        onStarted={(job) => showJob(queryClient, job)}
      />

      {projects.data.length === 0 ? (
        <p className="rounded-lg border border-dashed border-border p-8 text-center text-sm text-muted">
          No projects yet. Create one, or import a ZIP archive or a PDF.
        </p>
      ) : (
        <ul className="divide-y divide-border rounded-lg border border-border">
          {projects.data.map((project) => (
            <li
              key={project.id}
              className="group flex items-center gap-3 px-4 py-3"
            >
              <Link
                href={`/project/${project.id}`}
                className="min-w-0 flex-1 focus-visible:outline-2 focus-visible:outline-accent"
              >
                <span className="block truncate font-medium group-hover:text-accent">
                  {project.name}
                </span>
                <span className="block text-xs text-muted">
                  {project.mainFile} · edited{" "}
                  {formatRelative(project.updatedAt)}
                </span>
              </Link>
              <div className="flex gap-1 opacity-60 group-hover:opacity-100">
                <IconButton
                  label="Rename"
                  onClick={() => {
                    const newName = window.prompt(
                      "Rename project",
                      project.name,
                    );
                    if (newName?.trim() && newName !== project.name) {
                      rename.mutate({ id: project.id, newName });
                    }
                  }}
                >
                  <Pencil className="size-4" />
                </IconButton>
                <DownloadMenu projectId={project.id} />
                <MembersButton
                  projectId={project.id}
                  projectName={project.name}
                />
                <IconButton
                  label="Delete"
                  className="hover:text-danger"
                  onClick={() => {
                    if (
                      window.confirm(
                        `Delete "${project.name}" and all its files? This cannot be undone.`,
                      )
                    ) {
                      remove.mutate(project.id);
                    }
                  }}
                >
                  <Trash2 className="size-4" />
                </IconButton>
              </div>
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}

const RELATIVE = new Intl.RelativeTimeFormat("en", { numeric: "auto" });

export function formatRelative(ms: number): string {
  const seconds = Math.round((ms - Date.now()) / 1000);
  const units: Array<[Intl.RelativeTimeFormatUnit, number]> = [
    ["year", 31_536_000],
    ["month", 2_592_000],
    ["day", 86_400],
    ["hour", 3_600],
    ["minute", 60],
  ];
  for (const [unit, size] of units) {
    if (Math.abs(seconds) >= size) {
      return RELATIVE.format(Math.round(seconds / size), unit);
    }
  }
  return "just now";
}
