"use client";

import { useQuery } from "@tanstack/react-query";
import { ChevronDown, CircleUserRound } from "lucide-react";
import Link from "next/link";
import { useEffect, useRef, useState } from "react";
import { apiFetch } from "@/lib/api/client";
import { fetchSession, SESSION_KEY } from "@/lib/auth";
import { ChangePasswordDialog } from "./change-password-dialog";
import { SessionsDialog } from "./sessions-dialog";

type Open = "password" | "sessions" | null;

/**
 * Server mode: who is signed in, with their password, their sessions and signing out.
 * Single-user mode shows nothing.
 */
export function UserMenu({ compact = false }: { compact?: boolean }) {
  const session = useQuery({ queryKey: SESSION_KEY, queryFn: fetchSession });
  const [menu, setMenu] = useState(false);
  const [dialog, setDialog] = useState<Open>(null);
  const root = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!menu) return;
    const close = (event: MouseEvent) => {
      if (!root.current?.contains(event.target as Node)) setMenu(false);
    };
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") setMenu(false);
    };
    window.addEventListener("mousedown", close);
    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("mousedown", close);
      window.removeEventListener("keydown", onKey);
    };
  }, [menu]);

  const user = session.data?.mode === "server" ? session.data.user : null;
  if (!user) return null;

  const choose = (next: Open) => {
    setMenu(false);
    setDialog(next);
  };
  const signOut = async () => {
    setMenu(false);
    try {
      await apiFetch("/api/auth/logout", { method: "POST" });
    } finally {
      window.location.assign("/login");
    }
  };
  const item = "px-3 py-1.5 text-left hover:bg-panel";

  return (
    <div ref={root} className="relative">
      <button
        type="button"
        aria-haspopup="menu"
        aria-expanded={menu}
        title={`Signed in as ${user.username}`}
        onClick={() => setMenu((value) => !value)}
        className="inline-flex items-center gap-1.5 rounded-md px-2 py-1.5 text-sm hover:bg-zinc-200/60 dark:hover:bg-zinc-800/60"
      >
        <CircleUserRound className="size-4" />
        {!compact && (
          <span className="max-w-40 truncate">{user.displayName}</span>
        )}
        <ChevronDown className="size-3 text-muted" />
      </button>
      {menu && (
        <div
          role="menu"
          aria-label="Account"
          className="absolute top-9 right-0 z-30 flex w-56 flex-col rounded-md border border-border bg-background py-1 text-sm shadow-lg"
        >
          <div className="border-b border-border px-3 pt-1 pb-2">
            <p className="truncate font-medium">{user.displayName}</p>
            <p className="truncate text-xs text-muted">
              {user.username}
              {user.role === "admin" ? " · admin" : ""}
            </p>
          </div>
          {user.role === "admin" && (
            <Link
              role="menuitem"
              href="/admin"
              onClick={() => setMenu(false)}
              className={item}
            >
              Manage the server
            </Link>
          )}
          <button
            type="button"
            role="menuitem"
            onClick={() => choose("password")}
            className={item}
          >
            Change password
          </button>
          <button
            type="button"
            role="menuitem"
            onClick={() => choose("sessions")}
            className={item}
          >
            Signed-in devices
          </button>
          <button
            type="button"
            role="menuitem"
            onClick={() => void signOut()}
            className={`${item} border-t border-border`}
          >
            Sign out
          </button>
        </div>
      )}
      <ChangePasswordDialog
        open={dialog === "password"}
        onClose={() => setDialog(null)}
      />
      <SessionsDialog
        open={dialog === "sessions"}
        onClose={() => setDialog(null)}
      />
    </div>
  );
}
