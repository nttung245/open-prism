"use client";

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { FolderKanban, Link2, Lock, LockOpen, Plus } from "lucide-react";
import { type SubmitEvent, useState } from "react";
import { Button } from "@/components/ui/button";
import { FIELD } from "@/components/ui/modal";
import { apiJson, errorMessage } from "@/lib/api/client";
import type { CreateUserRequest } from "@/lib/api/types/CreateUserRequest";
import type { LinkResponse } from "@/lib/api/types/LinkResponse";
import type { MemberView } from "@/lib/api/types/MemberView";
import type { UpdateUserRequest } from "@/lib/api/types/UpdateUserRequest";
import type { UserProjectView } from "@/lib/api/types/UserProjectView";
import type { UserRole } from "@/lib/api/types/UserRole";
import type { UserView } from "@/lib/api/types/UserView";
import type { UserWithLink } from "@/lib/api/types/UserWithLink";
import { usernameProblem } from "@/lib/auth";
import { CopyLink } from "./copy-link";

const USERS = ["users"] as const;

function day(ms: number | null): string {
  return ms === null
    ? "never"
    : new Date(ms).toLocaleDateString(undefined, { dateStyle: "medium" });
}

/** The accounts of the server: adding people, locking them, roles and set-password links. */
export function UsersSection({ me }: { me: string }) {
  const users = useQuery({
    queryKey: USERS,
    queryFn: () => apiJson<UserView[]>("/api/users"),
  });
  const active = users.data?.filter((user) => !user.disabled) ?? [];

  return (
    <section className="flex flex-col gap-4">
      <div>
        <h2 className="text-lg font-semibold">People</h2>
        <p className="text-sm text-muted">
          Nobody signs up by themselves: add people here and send each one the
          link to set their password. A locked account cannot sign in, and its
          sessions end at once.
        </p>
      </div>
      {users.error && (
        <p className="text-sm text-danger">{errorMessage(users.error)}</p>
      )}
      <ul className="flex flex-col divide-y divide-border rounded-lg border border-border">
        {users.data?.map((user) => (
          <UserRow
            key={user.id}
            user={user}
            isMe={user.id === me}
            others={active.filter((other) => other.id !== user.id)}
          />
        ))}
      </ul>
      <AddUser />
    </section>
  );
}

function AddUser() {
  const client = useQueryClient();
  const [username, setUsername] = useState("");
  const [displayName, setDisplayName] = useState("");
  const [role, setRole] = useState<UserRole>("user");
  const [problem, setProblem] = useState<string | null>(null);
  const create = useMutation({
    mutationFn: (body: CreateUserRequest) =>
      apiJson<UserWithLink>("/api/users", { method: "POST", json: body }),
    onSuccess: () => {
      setUsername("");
      setDisplayName("");
      setRole("user");
      void client.invalidateQueries({ queryKey: USERS });
    },
  });
  const onSubmit = (event: SubmitEvent<HTMLFormElement>) => {
    event.preventDefault();
    const found =
      usernameProblem(username) ??
      (displayName.trim() ? null : "Enter the person's name.");
    setProblem(found);
    if (!found) {
      create.mutate({
        username: username.trim(),
        displayName: displayName.trim(),
        role,
      });
    }
  };
  const error = problem ?? (create.error ? errorMessage(create.error) : null);

  return (
    <form
      onSubmit={onSubmit}
      noValidate
      className="grid gap-2 rounded-lg border border-dashed border-border p-4 sm:grid-cols-[1fr_1fr_7rem]"
    >
      <p className="text-sm font-medium sm:col-span-3">Add a person</p>
      <label className="flex flex-col gap-1 text-xs text-muted">
        User name
        <input
          name="new-username"
          autoComplete="off"
          autoCapitalize="none"
          spellCheck={false}
          value={username}
          onChange={(event) => setUsername(event.target.value)}
          className={FIELD}
        />
      </label>
      <label className="flex flex-col gap-1 text-xs text-muted">
        Name
        <input
          name="new-display-name"
          autoComplete="off"
          value={displayName}
          onChange={(event) => setDisplayName(event.target.value)}
          className={FIELD}
        />
      </label>
      <label className="flex flex-col gap-1 text-xs text-muted">
        Role
        <select
          name="new-role"
          value={role}
          onChange={(event) => setRole(event.target.value as UserRole)}
          className={FIELD}
        >
          <option value="user">User</option>
          <option value="admin">Admin</option>
        </select>
      </label>
      {error && (
        <p role="alert" className="text-xs text-danger sm:col-span-3">
          {error}
        </p>
      )}
      {create.data && (
        <div className="sm:col-span-3">
          <CopyLink
            link={create.data.link}
            note={`Send this link to ${create.data.user.displayName}: it sets their password, works once, and expires in 7 days.`}
          />
        </div>
      )}
      <div className="sm:col-span-3">
        <Button type="submit" variant="primary" disabled={create.isPending}>
          <Plus className="size-4" /> Add
        </Button>
      </div>
    </form>
  );
}

function UserRow({
  user,
  isMe,
  others,
}: {
  user: UserView;
  isMe: boolean;
  others: UserView[];
}) {
  const client = useQueryClient();
  const [projectsOpen, setProjectsOpen] = useState(false);
  const update = useMutation({
    mutationFn: (body: UpdateUserRequest) =>
      apiJson<UserView>(`/api/users/${user.id}`, {
        method: "PATCH",
        json: body,
      }),
    onSuccess: () => void client.invalidateQueries({ queryKey: USERS }),
  });
  const link = useMutation({
    mutationFn: () =>
      apiJson<LinkResponse>(`/api/users/${user.id}/link`, { method: "POST" }),
  });
  const error = update.error ?? link.error;

  return (
    <li className="flex flex-col gap-2 px-4 py-3">
      <div className="flex flex-wrap items-center gap-2">
        <div className="min-w-0 flex-1">
          <p className="truncate text-sm font-medium">
            {user.displayName}
            {isMe && (
              <span className="ml-2 text-xs font-normal text-muted">you</span>
            )}
            {user.disabled && (
              <span className="ml-2 rounded bg-red-100 px-1.5 py-0.5 text-xs font-normal text-danger dark:bg-red-950/50">
                Locked
              </span>
            )}
            {!user.hasPassword && !user.disabled && (
              <span className="ml-2 rounded bg-panel px-1.5 py-0.5 text-xs font-normal text-muted">
                No password yet
              </span>
            )}
          </p>
          <p className="truncate text-xs text-muted">
            {user.username} · added {day(user.createdAt)} · last sign-in{" "}
            {day(user.lastLoginAt)}
          </p>
        </div>
        <select
          aria-label={`Role of ${user.username}`}
          value={user.role}
          disabled={update.isPending}
          onChange={(event) =>
            update.mutate({ role: event.target.value as UserRole })
          }
          className="rounded-md border border-border bg-background px-1.5 py-1 text-xs"
        >
          <option value="user">User</option>
          <option value="admin">Admin</option>
        </select>
        {!user.disabled && (
          <Button
            disabled={link.isPending}
            onClick={() => link.mutate()}
            title="A new link to set the password; older links stop working"
          >
            <Link2 className="size-4" /> New link
          </Button>
        )}
        {user.disabled && (
          <Button
            onClick={() => setProjectsOpen((value) => !value)}
            aria-expanded={projectsOpen}
          >
            <FolderKanban className="size-4" /> Projects
          </Button>
        )}
        <Button
          variant={user.disabled ? "secondary" : "danger"}
          disabled={update.isPending}
          onClick={() => {
            if (
              user.disabled ||
              window.confirm(
                `Lock ${user.displayName}? They are signed out everywhere and cannot sign in until unlocked.`,
              )
            ) {
              update.mutate({ disabled: !user.disabled });
            }
          }}
        >
          {user.disabled ? (
            <LockOpen className="size-4" />
          ) : (
            <Lock className="size-4" />
          )}
          {user.disabled ? "Unlock" : "Lock"}
        </Button>
      </div>
      {error && (
        <p role="alert" className="text-xs text-danger">
          {errorMessage(error)}
        </p>
      )}
      {link.data && !user.disabled && (
        <CopyLink
          link={link.data.link}
          note={`Send this link to ${user.displayName}: it sets a new password, works once, and expires in 7 days. Older links no longer work.`}
        />
      )}
      {user.disabled && projectsOpen && (
        <LockedProjects user={user} others={others} />
      )}
    </li>
  );
}

/** The projects of a locked account: the ones it owns can go to someone else. */
function LockedProjects({
  user,
  others,
}: {
  user: UserView;
  others: UserView[];
}) {
  const key = ["users", user.id, "projects"];
  const projects = useQuery({
    queryKey: key,
    queryFn: () => apiJson<UserProjectView[]>(`/api/users/${user.id}/projects`),
  });
  if (projects.isPending) {
    return <p className="text-xs text-muted">Loading projects…</p>;
  }
  if (projects.error) {
    return (
      <p className="text-xs text-danger">{errorMessage(projects.error)}</p>
    );
  }
  if (projects.data.length === 0) {
    return <p className="text-xs text-muted">Not in any project.</p>;
  }
  return (
    <ul className="flex flex-col gap-1 rounded-md bg-panel p-2">
      {projects.data.map((project) => (
        <LockedProject
          key={project.id}
          user={user}
          project={project}
          others={others}
          queryKey={key}
        />
      ))}
    </ul>
  );
}

function LockedProject({
  user,
  project,
  others,
  queryKey,
}: {
  user: UserView;
  project: UserProjectView;
  others: UserView[];
  queryKey: string[];
}) {
  const client = useQueryClient();
  const [target, setTarget] = useState("");
  const handOver = useMutation({
    mutationFn: (userId: string) =>
      apiJson<MemberView[]>(
        `/api/users/${user.id}/projects/${project.id}/owner`,
        { method: "POST", json: { userId } },
      ),
    onSuccess: () => void client.invalidateQueries({ queryKey }),
  });
  return (
    <li className="flex flex-wrap items-center gap-2 text-sm">
      <span className="min-w-0 flex-1 truncate">
        {project.name}{" "}
        <span className="text-xs text-muted">({project.role})</span>
      </span>
      {project.role === "owner" && (
        <>
          <select
            aria-label={`New owner of ${project.name}`}
            value={target}
            onChange={(event) => setTarget(event.target.value)}
            className="rounded-md border border-border bg-background px-1.5 py-1 text-xs"
          >
            <option value="">Hand over to…</option>
            {others.map((other) => (
              <option key={other.id} value={other.id}>
                {other.displayName} ({other.username})
              </option>
            ))}
          </select>
          <Button
            disabled={!target || handOver.isPending}
            onClick={() => handOver.mutate(target)}
          >
            Hand over
          </Button>
        </>
      )}
      {handOver.error && (
        <p className="w-full text-xs text-danger">
          {errorMessage(handOver.error)}
        </p>
      )}
    </li>
  );
}
