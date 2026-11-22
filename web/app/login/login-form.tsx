"use client";

import { useQuery } from "@tanstack/react-query";
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
import { fetchSession, SESSION_KEY, safeNext } from "@/lib/auth";

const INPUT =
  "rounded-md border border-zinc-300 bg-white px-3 py-2 text-sm outline-none focus:border-zinc-500 dark:border-zinc-700 dark:bg-zinc-900";

/**
 * Single-user mode signs in with the access token (or a `/login?token=…` link); server mode
 * with a user name and password.
 */
export function LoginForm() {
  const router = useRouter();
  const params = useSearchParams();
  const nextPath = safeNext(params.get("next"));
  const session = useQuery({
    queryKey: SESSION_KEY,
    queryFn: fetchSession,
    retry: false,
  });
  const [error, setError] = useState<string | null>(null);
  const [pending, setPending] = useState(false);

  const signIn = useCallback(
    async (body: LoginRequest) => {
      setPending(true);
      setError(null);
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

  if (session.isPending) {
    return <p className="text-sm text-muted">Loading…</p>;
  }
  if (session.isError) {
    return (
      <p role="alert" className="text-sm text-red-600 dark:text-red-400">
        Cannot reach the Open Prism server. Is it running?
      </p>
    );
  }
  const form =
    session.data.mode === "server" ? (
      <PasswordForm onSignIn={signIn} pending={pending} />
    ) : (
      <TokenForm onSignIn={signIn} pending={pending} />
    );
  return (
    <>
      {form}
      {error && (
        <p role="alert" className="text-sm text-red-600 dark:text-red-400">
          {error}
        </p>
      )}
    </>
  );
}

type FormProps = {
  onSignIn: (body: LoginRequest) => Promise<void>;
  pending: boolean;
};

function Title({ children }: { children: React.ReactNode }) {
  return (
    <div>
      <h1 className="text-2xl font-semibold tracking-tight">
        Sign in to Open Prism
      </h1>
      <p className="mt-2 text-sm text-zinc-600 dark:text-zinc-400">
        {children}
      </p>
    </div>
  );
}

function Submit({ pending }: { pending: boolean }) {
  return (
    <button
      type="submit"
      disabled={pending}
      className="rounded-md bg-zinc-900 px-3 py-2 text-sm font-medium text-white disabled:opacity-60 dark:bg-zinc-100 dark:text-zinc-900"
    >
      {pending ? "Signing in…" : "Sign in"}
    </button>
  );
}

function TokenForm({ onSignIn, pending }: FormProps) {
  const params = useSearchParams();
  const [token, setToken] = useState("");

  // A link such as /login?token=… signs in directly.
  const triedLink = useRef(false);
  useEffect(() => {
    const fromLink = params.get("token");
    if (fromLink && !triedLink.current) {
      triedLink.current = true;
      void onSignIn({ token: fromLink.trim() });
    }
  }, [params, onSignIn]);

  function onSubmit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    void onSignIn({ token: token.trim() });
  }

  return (
    <>
      <Title>
        Paste the access token printed by{" "}
        <code className="rounded bg-zinc-100 px-1 py-0.5 font-mono text-xs dark:bg-zinc-800">
          open-prism-api print-token
        </code>
        .
      </Title>
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
          className={`${INPUT} font-mono`}
        />
        <Submit pending={pending} />
      </form>
    </>
  );
}

function PasswordForm({ onSignIn, pending }: FormProps) {
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");

  function onSubmit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    void onSignIn({ username: username.trim(), password });
  }

  return (
    <>
      <Title>
        Use the account an admin made for you. Forgot your password? Ask an
        admin for a new link.
      </Title>
      <form onSubmit={onSubmit} className="flex flex-col gap-3">
        <label className="flex flex-col gap-1 text-sm">
          <span className="font-medium">User name</span>
          <input
            name="username"
            autoComplete="username"
            autoCapitalize="none"
            spellCheck={false}
            required
            value={username}
            onChange={(event) => setUsername(event.target.value)}
            className={INPUT}
          />
        </label>
        <label className="flex flex-col gap-1 text-sm">
          <span className="font-medium">Password</span>
          <input
            name="password"
            type="password"
            autoComplete="current-password"
            required
            value={password}
            onChange={(event) => setPassword(event.target.value)}
            className={INPUT}
          />
        </label>
        <Submit pending={pending} />
      </form>
    </>
  );
}
