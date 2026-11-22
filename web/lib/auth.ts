/**
 * Signing in, one-time links and passwords: the checks the pages make before calling the API.
 * The API makes the same checks and has the last word.
 */
import { apiJson } from "./api/client";
import type { SessionResponse } from "./api/types/SessionResponse";

export const SESSION_KEY = ["auth", "session"] as const;

/** Who is signed in, and how this Open Prism runs (public: works before signing in). */
export function fetchSession(): Promise<SessionResponse> {
  return apiJson<SessionResponse>("/api/auth/session");
}

/** As the API's `passwords::check_policy`. */
export const MIN_PASSWORD_CHARS = 12;
export const MAX_PASSWORD_BYTES = 1024;

/** Why a new password cannot be used, or `null` when it can. */
export function passwordProblem(
  password: string,
  confirmation: string,
): string | null {
  if ([...password].length < MIN_PASSWORD_CHARS) {
    return `Choose a password of at least ${MIN_PASSWORD_CHARS} characters.`;
  }
  if (new TextEncoder().encode(password).length > MAX_PASSWORD_BYTES) {
    return `A password can have at most ${MAX_PASSWORD_BYTES} bytes.`;
  }
  if (password !== confirmation) {
    return "The two passwords differ.";
  }
  return null;
}

/** As the API's `users::check_username`. */
export function usernameProblem(username: string): string | null {
  return /^[A-Za-z0-9._-]{1,64}$/.test(username.trim())
    ? null
    : "A user name has 1 to 64 letters, digits, '.', '_' or '-'.";
}

/** Only same-site relative paths, to avoid open redirects. */
export function safeNext(value: string | null | undefined): string {
  return value?.startsWith("/") && !value.startsWith("//") ? value : "/";
}

/** "Chrome on Linux" from a user agent, for the list of sessions. */
export function describeAgent(userAgent: string | null): string {
  if (!userAgent) return "Unknown device";
  const browser =
    [
      ["Edg/", "Edge"],
      ["OPR/", "Opera"],
      ["Firefox/", "Firefox"],
      ["Chrome/", "Chrome"],
      ["Safari/", "Safari"],
      ["curl/", "curl"],
    ].find(([needle]) => userAgent.includes(needle))?.[1] ?? "A browser";
  const system =
    [
      ["Android", "Android"],
      ["iPhone", "iOS"],
      ["iPad", "iPadOS"],
      ["Windows", "Windows"],
      ["Mac OS X", "macOS"],
      ["CrOS", "ChromeOS"],
      ["Linux", "Linux"],
    ].find(([needle]) => userAgent.includes(needle))?.[1] ?? null;
  return system ? `${browser} on ${system}` : browser;
}
