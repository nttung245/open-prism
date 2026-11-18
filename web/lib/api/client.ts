/**
 * Browser-side calls to the API. Requests go to `/api/*` on the same origin; the Next.js
 * server forwards them to the Rust API with the session cookie.
 */
import type { ErrorBody } from "./types/ErrorBody";

export class ApiError extends Error {
  constructor(
    readonly status: number,
    readonly code: string,
    message: string,
    readonly response?: Response,
  ) {
    super(message);
  }
}

/** Fetches `path` and throws an `ApiError` unless the response is 2xx. */
export async function apiFetch(
  path: `/api/${string}`,
  init?: RequestInit,
): Promise<Response> {
  let response: Response;
  try {
    response = await fetch(path, { cache: "no-store", ...init });
  } catch (error) {
    if (error instanceof DOMException && error.name === "AbortError") {
      throw error;
    }
    throw new ApiError(0, "network", `Network error: ${String(error)}`);
  }
  if (!response.ok) {
    const body = (await response
      .clone()
      .json()
      .catch(() => null)) as ErrorBody | null;
    if (response.status === 401 && typeof window !== "undefined") {
      // The session expired or the token changed: sign in again, then come back.
      const next = window.location.pathname + window.location.search;
      window.location.assign(`/login?next=${encodeURIComponent(next)}`);
    }
    throw new ApiError(
      response.status,
      body?.error.code ?? "unknown",
      body?.error.message ?? `HTTP ${response.status}`,
      response,
    );
  }
  return response;
}

export async function apiJson<T>(
  path: `/api/${string}`,
  init?: RequestInit & { json?: unknown },
): Promise<T> {
  const { json, ...rest } = init ?? {};
  const response = await apiFetch(path, {
    ...rest,
    headers:
      json === undefined
        ? rest.headers
        : { "content-type": "application/json", ...rest.headers },
    body: json === undefined ? rest.body : JSON.stringify(json),
  });
  if (response.status === 204) return undefined as T;
  return (await response.json()) as T;
}

/** URL of a project file, with each path segment encoded. */
export function fileUrl(projectId: string, path: string): `/api/${string}` {
  return `/api/projects/${projectId}/files/${encodePath(path)}`;
}

export function encodePath(path: string): string {
  return path.split("/").map(encodeURIComponent).join("/");
}

/** Strips the quotes of an `ETag` header value. */
export function etagVersion(response: Response): string | null {
  const etag = response.headers.get("etag");
  return etag ? etag.replace(/^W\//, "").replace(/"/g, "") : null;
}

export function errorMessage(error: unknown): string {
  if (error instanceof ApiError) return error.message;
  if (error instanceof Error) return error.message;
  return String(error);
}
