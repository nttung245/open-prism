"use client";

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Button } from "@/components/ui/button";
import { Modal } from "@/components/ui/modal";
import { apiFetch, apiJson } from "@/lib/api/client";
import type { SessionView } from "@/lib/api/types/SessionView";
import { describeAgent } from "@/lib/auth";

const SESSIONS_KEY = ["auth", "sessions"] as const;

function when(ms: number): string {
  return new Date(ms).toLocaleString(undefined, {
    dateStyle: "medium",
    timeStyle: "short",
  });
}

/** The devices signed in to the account, each of which can be signed out. */
export function SessionsDialog({
  open,
  onClose,
}: {
  open: boolean;
  onClose: () => void;
}) {
  const client = useQueryClient();
  const sessions = useQuery({
    queryKey: SESSIONS_KEY,
    queryFn: () => apiJson<SessionView[]>("/api/auth/sessions"),
    enabled: open,
  });
  const revoke = useMutation({
    mutationFn: (id: string) =>
      apiFetch(`/api/auth/sessions/${encodeURIComponent(id)}`, {
        method: "DELETE",
      }),
    onSettled: () => client.invalidateQueries({ queryKey: SESSIONS_KEY }),
  });

  return (
    <Modal
      open={open}
      onClose={onClose}
      title="Signed-in devices"
      description="Sign out a device you no longer use. Sessions end by themselves after 30 days without use."
    >
      {sessions.isPending && <p className="text-sm text-muted">Loading…</p>}
      {sessions.isError && (
        <p role="alert" className="text-sm text-danger">
          {sessions.error.message}
        </p>
      )}
      {sessions.data && (
        <ul className="flex flex-col divide-y divide-border rounded-md border border-border">
          {sessions.data.map((session) => (
            <li
              key={session.id}
              className="flex items-center justify-between gap-3 px-3 py-2"
            >
              <div className="min-w-0 text-sm">
                <p className="truncate font-medium">
                  {describeAgent(session.userAgent)}
                  {session.current && (
                    <span className="ml-2 rounded bg-panel px-1.5 py-0.5 text-xs font-normal text-muted">
                      This device
                    </span>
                  )}
                </p>
                <p className="text-xs text-muted">
                  Last used {when(session.lastUsedAt)} · signed in{" "}
                  {when(session.createdAt)}
                </p>
              </div>
              {!session.current && (
                <Button
                  variant="danger"
                  disabled={revoke.isPending}
                  onClick={() => revoke.mutate(session.id)}
                >
                  Sign out
                </Button>
              )}
            </li>
          ))}
        </ul>
      )}
      {revoke.isError && (
        <p role="alert" className="text-sm text-danger">
          {revoke.error.message}
        </p>
      )}
    </Modal>
  );
}
