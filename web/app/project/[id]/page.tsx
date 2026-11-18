import { notFound } from "next/navigation";
import { Suspense } from "react";
import { WorkspaceLoader } from "@/components/workspace/workspace-loader";
import { ApiError, apiGet } from "@/lib/api/server";
import type { Project } from "@/lib/api/types/Project";

export default function ProjectPage({ params }: PageProps<"/project/[id]">) {
  return (
    <Suspense fallback={<WorkspaceSkeleton />}>
      {params.then(({ id }) => (
        <ProjectWorkspace id={id} />
      ))}
    </Suspense>
  );
}

async function ProjectWorkspace({ id }: { id: string }) {
  let project: Project;
  try {
    project = await apiGet<Project>(`/api/projects/${encodeURIComponent(id)}`);
  } catch (error) {
    if (error instanceof ApiError && error.status === 404) notFound();
    throw error;
  }
  return <WorkspaceLoader project={project} />;
}

function WorkspaceSkeleton() {
  return (
    <div className="flex h-full flex-col">
      <div className="h-11 border-b border-border" />
      <p className="m-auto text-sm text-muted">Opening the project…</p>
    </div>
  );
}
