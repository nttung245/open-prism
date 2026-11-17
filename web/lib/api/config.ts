/** Where the Rust API listens. Read at run time on the server only. */
export const DEFAULT_API_URL = "http://127.0.0.1:8787";

export function apiBaseUrl(): string {
  return process.env.OPENPRISM_API_URL ?? DEFAULT_API_URL;
}

/** Name of the session cookie issued by `POST /api/auth/login`. */
export const SESSION_COOKIE = "openprism_session";
