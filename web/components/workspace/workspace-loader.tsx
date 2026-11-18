"use client";

import dynamic from "next/dynamic";
import type { Project } from "@/lib/api/types/Project";

// CodeMirror and pdf.js need the browser: load the workspace on the client only.
const Workspace = dynamic(() => import("./workspace"), {
  ssr: false,
  loading: () => (
    <p className="m-auto text-sm text-muted">Loading the editor…</p>
  ),
});

export function WorkspaceLoader({ project }: { project: Project }) {
  return <Workspace initialProject={project} />;
}
