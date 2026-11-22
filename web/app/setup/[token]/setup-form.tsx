"use client";

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useParams, useRouter } from "next/navigation";
import { type SubmitEvent, useId, useState } from "react";
import { ApiError, apiFetch, apiJson } from "@/lib/api/client";
import type { LinkInfo } from "@/lib/api/types/LinkInfo";
import type { UseLinkRequest } from "@/lib/api/types/UseLinkRequest";
import { passwordProblem, SESSION_KEY, usernameProblem } from "@/lib/auth";

const INPUT =
  "rounded-md border border-zinc-300 bg-white px-3 py-2 text-sm outline-none focus:border-zinc-500 dark:border-zinc-700 dark:bg-zinc-900";

/**
 * A one-time link: the first admin creates their account here, and people an admin added set
 * their password. Either way they are signed in afterwards.
 */
export function SetupForm() {
  const { token } = useParams<{ token: string }>();
  const router = useRouter();
  const queryClient = useQueryClient();
  const path = `/api/auth/links/${encodeURIComponent(token)}` as const;
  const link = useQuery({
    queryKey: ["auth", "link", token],
    queryFn: () => apiJson<LinkInfo>(path),
    retry: false,
  });
  const [username, setUsername] = useState("");
  const [displayName, setDisplayName] = useState("");
  const [password, setPassword] = useState("");
  const [confirmation, setConfirmation] = useState("");
  const [problem, setProblem] = useState<string | null>(null);
  // Hints stay out of the labels, so a field's name is only its label.
  const hint = useId();
  const submit = useMutation({
    mutationFn: (body: UseLinkRequest) =>
      apiFetch(path, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify(body),
      }),
    onSuccess: () => {
      queryClient.removeQueries({ queryKey: SESSION_KEY });
      router.replace("/");
      router.refresh();
    },
  });

  if (link.isPending) return <p className="text-sm text-muted">Loading…</p>;
  if (link.isError) {
    return (
      <div className="flex flex-col gap-2">
        <h1 className="text-2xl font-semibold tracking-tight">
          This link does not work
        </h1>
        <p role="alert" className="text-sm text-zinc-600 dark:text-zinc-400">
          {link.error instanceof ApiError && link.error.status === 404
            ? link.error.message
            : "Cannot reach the Open Prism server. Is it running?"}
        </p>
      </div>
    );
  }

  const firstAdmin = link.data.kind === "firstAdmin";
  function onSubmit(event: SubmitEvent<HTMLFormElement>) {
    event.preventDefault();
    const found =
      (firstAdmin ? usernameProblem(username) : null) ??
      (firstAdmin && !displayName.trim() ? "Enter your name." : null) ??
      passwordProblem(password, confirmation);
    setProblem(found);
    if (found) return;
    submit.mutate(
      firstAdmin
        ? {
            username: username.trim(),
            displayName: displayName.trim(),
            password,
          }
        : { password },
    );
  }

  const error =
    problem ?? (submit.error instanceof Error ? submit.error.message : null);
  return (
    <>
      <div>
        <h1 className="text-2xl font-semibold tracking-tight">
          {firstAdmin ? "Create the first admin" : "Set your password"}
        </h1>
        <p className="mt-2 text-sm text-zinc-600 dark:text-zinc-400">
          {firstAdmin ? (
            <>
              This account manages the server: it adds people and shares AI
              providers. Projects already on this server become yours.
            </>
          ) : (
            <>
              For <b>{link.data.displayName}</b> (user name{" "}
              <code className="font-mono">{link.data.username}</code>). This
              link works once.
            </>
          )}
        </p>
      </div>
      <form onSubmit={onSubmit} className="flex flex-col gap-3" noValidate>
        {firstAdmin && (
          <>
            <label className="flex flex-col gap-1 text-sm">
              <span className="font-medium">User name</span>
              <input
                name="username"
                autoComplete="username"
                autoCapitalize="none"
                spellCheck={false}
                value={username}
                onChange={(event) => setUsername(event.target.value)}
                className={INPUT}
              />
            </label>
            <div className="flex flex-col gap-1 text-sm">
              <label className="flex flex-col gap-1">
                <span className="font-medium">Your name</span>
                <input
                  name="displayName"
                  autoComplete="name"
                  aria-describedby={`${hint}-name`}
                  value={displayName}
                  onChange={(event) => setDisplayName(event.target.value)}
                  className={INPUT}
                />
              </label>
              <span id={`${hint}-name`} className="text-xs text-muted">
                Shown to others next to your changes.
              </span>
            </div>
          </>
        )}
        {!firstAdmin && (
          // Lets password managers save the password under the right account.
          <input
            type="hidden"
            name="username"
            autoComplete="username"
            value={link.data.username ?? ""}
          />
        )}
        <div className="flex flex-col gap-1 text-sm">
          <label className="flex flex-col gap-1">
            <span className="font-medium">Password</span>
            <input
              name="password"
              type="password"
              autoComplete="new-password"
              aria-describedby={`${hint}-password`}
              value={password}
              onChange={(event) => setPassword(event.target.value)}
              className={INPUT}
            />
          </label>
          <span id={`${hint}-password`} className="text-xs text-muted">
            At least 12 characters.
          </span>
        </div>
        <label className="flex flex-col gap-1 text-sm">
          <span className="font-medium">Password again</span>
          <input
            name="confirmation"
            type="password"
            autoComplete="new-password"
            value={confirmation}
            onChange={(event) => setConfirmation(event.target.value)}
            className={INPUT}
          />
        </label>
        {error && (
          <p role="alert" className="text-sm text-red-600 dark:text-red-400">
            {error}
          </p>
        )}
        <button
          type="submit"
          disabled={submit.isPending}
          className="rounded-md bg-zinc-900 px-3 py-2 text-sm font-medium text-white disabled:opacity-60 dark:bg-zinc-100 dark:text-zinc-900"
        >
          {submit.isPending
            ? "Saving…"
            : firstAdmin
              ? "Create the account"
              : "Set the password"}
        </button>
      </form>
    </>
  );
}
