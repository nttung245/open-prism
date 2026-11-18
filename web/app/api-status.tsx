import { ApiError, apiGet } from "@/lib/api/server";
import type { HealthResponse } from "@/lib/api/types/HealthResponse";

/** One line about the API and the TeX engines it found. */
export async function ApiStatus() {
  let health: HealthResponse;
  try {
    health = await apiGet<HealthResponse>("/api/health");
  } catch (error) {
    const message = error instanceof ApiError ? error.message : String(error);
    return <p className="text-xs text-danger">API unreachable: {message}</p>;
  }
  const engines = health.engines.map((engine) =>
    engine.runtime.type === "docker"
      ? `${engine.kind} in Docker (${engine.runtime.image})`
      : `${engine.kind} (${engine.runtime.path})`,
  );
  return (
    <p className="text-xs text-muted">
      API v{health.version} ·{" "}
      {engines.length > 0 ? (
        engines.join(", ")
      ) : (
        <span className="text-warning">
          No TeX engine found. Run <code>just tex-pull</code> to download TeX
          Live with Docker, or install latexmk or Tectonic.
        </span>
      )}
    </p>
  );
}
