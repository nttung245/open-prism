import { Settings } from "lucide-react";
import Link from "next/link";
import { Suspense } from "react";
import { ProjectList } from "@/components/projects/project-list";
import { apiGet } from "@/lib/api/server";
import type { Project } from "@/lib/api/types/Project";
import { ApiStatus } from "./api-status";

export default function Home() {
  return (
    <main className="mx-auto flex w-full max-w-3xl flex-1 flex-col gap-8 px-6 py-12">
      <header className="flex flex-wrap items-end justify-between gap-4">
        <div>
          <h1 className="text-2xl font-semibold tracking-tight">Open Prism</h1>
          <p className="mt-1 text-sm text-muted">
            LaTeX editor with AI assistance that works with any LLM provider.
          </p>
        </div>
        <Link
          href="/settings"
          className="inline-flex items-center gap-1.5 rounded-md border border-border px-2.5 py-1.5 text-sm hover:bg-panel"
        >
          <Settings className="size-4" /> Settings
        </Link>
      </header>
      <Suspense
        fallback={<p className="text-sm text-muted">Loading projects…</p>}
      >
        <Projects />
      </Suspense>
      <footer className="mt-auto">
        <Suspense fallback={null}>
          <ApiStatus />
        </Suspense>
      </footer>
    </main>
  );
}

async function Projects() {
  const projects = await apiGet<Project[]>("/api/projects");
  return <ProjectList initial={projects} />;
}
