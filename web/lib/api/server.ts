/**
 * Calls the Rust API from Server Components, forwarding the visitor's cookies so the API
 * sees the same session as the browser.
 */
import { cookies } from "next/headers";
import { apiBaseUrl } from "./config";
import type { ErrorBody } from "./types/ErrorBody";

export class ApiError extends Error {
  constructor(
    readonly status: number,
    readonly code: string,
    message: string,
  ) {
    super(message);
  }
}

export async function apiGet<T>(path: `/api/${string}`): Promise<T> {
  const cookieHeader = (await cookies()).toString();
  let response: Response;
  try {
    response = await fetch(new URL(path, apiBaseUrl()), {
      headers: cookieHeader ? { cookie: cookieHeader } : undefined,
    });
  } catch (error) {
    throw new ApiError(
      502,
      "api_unavailable",
      `Cannot reach the API: ${String(error)}`,
    );
  }
  if (!response.ok) {
    const body = (await response.json().catch(() => null)) as ErrorBody | null;
    throw new ApiError(
      response.status,
      body?.error.code ?? "unknown",
      body?.error.message ?? `HTTP ${response.status}`,
    );
  }
  return (await response.json()) as T;
}
