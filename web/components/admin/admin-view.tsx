"use client";

import { useQuery } from "@tanstack/react-query";
import { ArrowLeft } from "lucide-react";
import Link from "next/link";
import { AiSettings } from "@/components/settings/settings-view";
import { errorMessage } from "@/lib/api/client";
import { fetchSession, SESSION_KEY } from "@/lib/auth";
import { UsersSection } from "./users-section";

/** Server mode, admins only: accounts, shared AI providers and the default models. */
export function AdminView() {
  const session = useQuery({ queryKey: SESSION_KEY, queryFn: fetchSession });
  const user = session.data?.user ?? null;

  let body: React.ReactNode;
  if (session.isPending) {
    body = <p className="text-sm text-muted">Loading…</p>;
  } else if (session.error) {
    body = <p className="text-sm text-danger">{errorMessage(session.error)}</p>;
  } else if (session.data?.mode !== "server") {
    body = (
      <p className="text-sm text-muted">
        Accounts exist in server mode only. This Open Prism runs for one person
        on this computer.
      </p>
    );
  } else if (user?.role !== "admin") {
    body = (
      <p role="alert" className="text-sm text-danger">
        Only an admin can manage the server.
      </p>
    );
  } else {
    body = (
      <>
        <UsersSection me={user.id} />
        <AiSettings scope="shared" isAdmin />
      </>
    );
  }

  return (
    <main className="mx-auto flex w-full max-w-3xl flex-col gap-10 px-6 py-10">
      <header className="flex items-center gap-3">
        <Link
          href="/"
          aria-label="All projects"
          className="inline-flex size-8 items-center justify-center rounded-md text-muted hover:bg-zinc-200/60 hover:text-foreground dark:hover:bg-zinc-800/60"
        >
          <ArrowLeft className="size-4" />
        </Link>
        <h1 className="text-2xl font-semibold tracking-tight">
          Manage the server
        </h1>
      </header>
      {body}
    </main>
  );
}
