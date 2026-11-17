"use client";

import { useRouter, useSearchParams } from "next/navigation";
import {
  type FormEvent,
  useCallback,
  useEffect,
  useRef,
  useState,
} from "react";
import type { ErrorBody } from "@/lib/api/types/ErrorBody";
import type { LoginRequest } from "@/lib/api/types/LoginRequest";

/** Only same-site relative paths, to avoid open redirects. */
function safeNext(value: string | null): string {
  return value?.startsWith("/") && !value.startsWith("//") ? value : "/";
}

export function LoginForm() {
  const router = useRouter();
  const params = useSearchParams();
  const nextPath = safeNext(params.get("next"));
  const [token, setToken] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [pending, setPending] = useState(false);

  const signIn = useCallback(
    async (value: string) => {
      setPending(true);
      setError(null);
      const body: LoginRequest = { token: value.trim() };
      try {
        const response = await fetch("/api/auth/login", {
          method: "POST",
          headers: { "content-type": "application/json" },
          body: JSON.stringify(body),
        });
        if (response.ok) {
          // `replace` also drops a `?token=` link from the browser history.
          router.replace(nextPath);
          router.refresh();
          return;
        }
        const failure = (await response
          .json()
          .catch(() => null)) as ErrorBody | null;
        setError(
          failure?.error.message ?? `Sign-in failed (HTTP ${response.status})`,
        );
      } catch {
        setError("Cannot reach the server");
      } finally {
        setPending(false);
      }
    },
    [router, nextPath],
  );

  // A link such as /login?token=… signs in directly.
  const triedLink = useRef(false);
  useEffect(() => {
    const fromLink = params.get("token");
    if (fromLink && !triedLink.current) {
      triedLink.current = true;
      void signIn(fromLink);
    }
  }, [params, signIn]);

  function onSubmit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    void signIn(token);
  }

  return (
    <form onSubmit={onSubmit} className="flex flex-col gap-3">
      <label htmlFor="token" className="text-sm font-medium">
        Access token
      </label>
      <input
        id="token"
        type="password"
        autoComplete="current-password"
        required
        value={token}
        onChange={(event) => setToken(event.target.value)}
        className="rounded-md border border-zinc-300 bg-white px-3 py-2 font-mono text-sm outline-none focus:border-zinc-500 dark:border-zinc-700 dark:bg-zinc-900"
      />
      {error && (
        <p role="alert" className="text-sm text-red-600 dark:text-red-400">
          {error}
        </p>
      )}
      <button
        type="submit"
        disabled={pending}
        className="rounded-md bg-zinc-900 px-3 py-2 text-sm font-medium text-white disabled:opacity-60 dark:bg-zinc-100 dark:text-zinc-900"
      >
        {pending ? "Signing in…" : "Sign in"}
      </button>
    </form>
  );
}
