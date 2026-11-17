import { Suspense } from "react";
import { ApiStatus } from "./api-status";

export default function Home() {
  return (
    <main className="mx-auto flex w-full max-w-2xl flex-1 flex-col gap-8 px-6 py-16">
      <header>
        <h1 className="text-3xl font-semibold tracking-tight">Open Prism</h1>
        <p className="mt-2 text-zinc-600 dark:text-zinc-400">
          LaTeX editor with AI assistance that works with any LLM provider.
          Pre-alpha.
        </p>
      </header>
      <Suspense
        fallback={<p className="text-sm text-zinc-500">Checking the API…</p>}
      >
        <ApiStatus />
      </Suspense>
    </main>
  );
}
