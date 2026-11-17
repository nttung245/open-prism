import { ApiError, apiGet } from "@/lib/api/server";
import type { HealthResponse } from "@/lib/api/types/HealthResponse";

export async function ApiStatus() {
  let health: HealthResponse;
  try {
    health = await apiGet<HealthResponse>("/api/health");
  } catch (error) {
    const message = error instanceof ApiError ? error.message : String(error);
    return (
      <section className="rounded-lg border border-red-300 p-4 text-sm dark:border-red-900">
        <h2 className="font-medium text-red-700 dark:text-red-400">
          API unreachable
        </h2>
        <p className="mt-1 text-zinc-600 dark:text-zinc-400">{message}</p>
      </section>
    );
  }

  return (
    <section className="rounded-lg border border-zinc-200 p-4 text-sm dark:border-zinc-800">
      <h2 className="font-medium">API v{health.version}</h2>
      {health.engines.length === 0 ? (
        <p className="mt-1 text-amber-700 dark:text-amber-400">
          No TeX engine found. Run <code>just tex-pull</code> to download TeX
          Live with Docker, or install latexmk or Tectonic.
        </p>
      ) : (
        <ul className="mt-2 space-y-1">
          {health.engines.map((engine) => (
            <li
              key={`${engine.kind}-${engine.runtime.type}`}
              className="font-mono text-xs"
            >
              {engine.kind}: {engine.version ?? "unknown version"} (
              {engine.runtime.type === "docker"
                ? `docker ${engine.runtime.image}`
                : engine.runtime.path}
              )
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}
