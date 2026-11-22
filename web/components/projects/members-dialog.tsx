"use client";

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Crown, LogOut, UserMinus, UserPlus, Users } from "lucide-react";
import { useRouter } from "next/navigation";
import { type SubmitEvent, useState } from "react";
import { Button, IconButton } from "@/components/ui/button";
import { FIELD, Modal } from "@/components/ui/modal";
import { apiFetch, apiJson, errorMessage } from "@/lib/api/client";
import type { MemberView } from "@/lib/api/types/MemberView";
import type { ProjectRole } from "@/lib/api/types/ProjectRole";
import { fetchSession, SESSION_KEY } from "@/lib/auth";
import {
  actionsOn,
  canManagePeople,
  fetchMembers,
  membersKey,
  ROLE_LABELS,
  roleOf,
} from "@/lib/members";
import { useHydrated } from "@/lib/use-hydrated";

/** The Members button of a project; nothing in single-user mode. */
export function MembersButton({
  projectId,
  projectName,
}: {
  projectId: string;
  projectName: string;
}) {
  const session = useQuery({ queryKey: SESSION_KEY, queryFn: fetchSession });
  const [open, setOpen] = useState(false);
  const hydrated = useHydrated();
  if (!hydrated || session.data?.mode !== "server") return null;
  return (
    <>
      <IconButton label="Members" onClick={() => setOpen(true)}>
        <Users className="size-4" />
      </IconButton>
      <MembersDialog
        projectId={projectId}
        projectName={projectName}
        me={session.data.user?.id ?? null}
        open={open}
        onClose={() => setOpen(false)}
      />
    </>
  );
}

function MembersDialog({
  projectId,
  projectName,
  me,
  open,
  onClose,
}: {
  projectId: string;
  projectName: string;
  me: string | null;
  open: boolean;
  onClose: () => void;
}) {
  const router = useRouter();
  const client = useQueryClient();
  const key = membersKey(projectId);
  const members = useQuery({
    queryKey: key,
    queryFn: () => fetchMembers(projectId),
    enabled: open,
  });
  const myRole = roleOf(members.data ?? [], me);
  const settle = () => {
    void client.invalidateQueries({ queryKey: key });
    void client.invalidateQueries({ queryKey: ["projects"] });
  };
  const change = useMutation({
    mutationFn: ({ userId, role }: { userId: string; role: ProjectRole }) =>
      apiJson<MemberView[]>(`/api/projects/${projectId}/members/${userId}`, {
        method: "PATCH",
        json: { role },
      }),
    onSuccess: (list) => client.setQueryData(key, list),
  });
  const remove = useMutation({
    mutationFn: (userId: string) =>
      apiFetch(`/api/projects/${projectId}/members/${userId}`, {
        method: "DELETE",
      }),
    onSuccess: (_, userId) => {
      if (userId === me) {
        // Left the project: it is gone for this person.
        onClose();
        router.push("/");
      }
      settle();
    },
  });
  const handOver = useMutation({
    mutationFn: (userId: string) =>
      apiJson<MemberView[]>(`/api/projects/${projectId}/owner`, {
        method: "POST",
        json: { userId },
      }),
    onSuccess: (list) => {
      client.setQueryData(key, list);
      settle();
    },
  });
  const error = change.error ?? remove.error ?? handOver.error;

  return (
    <Modal
      open={open}
      onClose={onClose}
      title={`Members of ${projectName}`}
      description="Everyone here can open, edit and compile the project. Managers also add and remove people; only the owner deletes the project or hands it over."
      wide
    >
      {members.isPending && <p className="text-sm text-muted">Loading…</p>}
      {members.error && (
        <p role="alert" className="text-sm text-danger">
          {errorMessage(members.error)}
        </p>
      )}
      {members.data && (
        <ul className="flex flex-col divide-y divide-border rounded-md border border-border">
          {members.data.map((member) => {
            const can = actionsOn(myRole, me, member);
            const busy =
              change.isPending || remove.isPending || handOver.isPending;
            return (
              <li
                key={member.userId}
                className="flex flex-wrap items-center gap-2 px-3 py-2"
              >
                <div className="min-w-0 flex-1 text-sm">
                  <p className="truncate font-medium">
                    {member.displayName}
                    {member.userId === me && (
                      <span className="ml-2 text-xs font-normal text-muted">
                        you
                      </span>
                    )}
                  </p>
                  <p className="truncate text-xs text-muted">
                    {member.username}
                  </p>
                </div>
                {can.changeRole ? (
                  <select
                    aria-label={`Role of ${member.username}`}
                    value={member.role}
                    disabled={busy}
                    onChange={(event) =>
                      change.mutate({
                        userId: member.userId,
                        role: event.target.value as ProjectRole,
                      })
                    }
                    className="rounded-md border border-border bg-background px-1.5 py-1 text-xs"
                  >
                    <option value="member">{ROLE_LABELS.member}</option>
                    <option value="manager">{ROLE_LABELS.manager}</option>
                  </select>
                ) : (
                  <span className="inline-flex items-center gap-1 text-xs text-muted">
                    {member.role === "owner" && <Crown className="size-3.5" />}
                    {ROLE_LABELS[member.role]}
                  </span>
                )}
                {can.makeOwner && (
                  <Button
                    disabled={busy}
                    title="Hand the project over; you stay on as a manager"
                    onClick={() => {
                      if (
                        window.confirm(
                          `Make ${member.displayName} the owner of "${projectName}"? You become a manager.`,
                        )
                      ) {
                        handOver.mutate(member.userId);
                      }
                    }}
                  >
                    <Crown className="size-4" /> Make owner
                  </Button>
                )}
                {can.remove && (
                  <Button
                    variant="danger"
                    disabled={busy}
                    onClick={() => {
                      if (
                        window.confirm(
                          `Remove ${member.displayName} from "${projectName}"? Their AI conversations in it are deleted.`,
                        )
                      ) {
                        remove.mutate(member.userId);
                      }
                    }}
                  >
                    <UserMinus className="size-4" /> Remove
                  </Button>
                )}
                {can.leave && (
                  <Button
                    variant="danger"
                    disabled={busy}
                    onClick={() => {
                      if (
                        window.confirm(
                          `Leave "${projectName}"? Your AI conversations in it are deleted.`,
                        )
                      ) {
                        remove.mutate(member.userId);
                      }
                    }}
                  >
                    <LogOut className="size-4" /> Leave
                  </Button>
                )}
              </li>
            );
          })}
        </ul>
      )}
      {error && (
        <p role="alert" className="text-sm text-danger">
          {errorMessage(error)}
        </p>
      )}
      {canManagePeople(myRole) && <AddMember projectId={projectId} />}
    </Modal>
  );
}

function AddMember({ projectId }: { projectId: string }) {
  const client = useQueryClient();
  const [username, setUsername] = useState("");
  const [role, setRole] = useState<ProjectRole>("member");
  const add = useMutation({
    mutationFn: () =>
      apiJson<MemberView[]>(`/api/projects/${projectId}/members`, {
        method: "POST",
        json: { username: username.trim(), role },
      }),
    onSuccess: (list) => {
      setUsername("");
      client.setQueryData(membersKey(projectId), list);
    },
  });
  const onSubmit = (event: SubmitEvent<HTMLFormElement>) => {
    event.preventDefault();
    if (username.trim()) add.mutate();
  };
  return (
    <form onSubmit={onSubmit} className="flex flex-col gap-2">
      <div className="flex flex-wrap items-end gap-2">
        <label className="flex min-w-40 flex-1 flex-col gap-1 text-xs text-muted">
          Add someone by user name
          <input
            name="member-username"
            autoComplete="off"
            autoCapitalize="none"
            spellCheck={false}
            value={username}
            onChange={(event) => setUsername(event.target.value)}
            className={FIELD}
          />
        </label>
        <label className="flex flex-col gap-1 text-xs text-muted">
          Role
          <select
            name="member-role"
            value={role}
            onChange={(event) => setRole(event.target.value as ProjectRole)}
            className={FIELD}
          >
            <option value="member">{ROLE_LABELS.member}</option>
            <option value="manager">{ROLE_LABELS.manager}</option>
          </select>
        </label>
        <Button
          type="submit"
          variant="primary"
          disabled={add.isPending || !username.trim()}
        >
          <UserPlus className="size-4" /> Add
        </Button>
      </div>
      {add.error && (
        <p role="alert" className="text-xs text-danger">
          {errorMessage(add.error)}
        </p>
      )}
    </form>
  );
}
