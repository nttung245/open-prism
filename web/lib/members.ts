/**
 * The people of a project (server mode): what each person may do in the Members dialog. The API
 * enforces the same rules (`project-access` spec).
 */
import { apiJson } from "./api/client";
import type { MemberView } from "./api/types/MemberView";
import type { ProjectRole } from "./api/types/ProjectRole";

export function membersKey(projectId: string) {
  return ["members", projectId] as const;
}

export function fetchMembers(projectId: string): Promise<MemberView[]> {
  return apiJson<MemberView[]>(`/api/projects/${projectId}/members`);
}

/** The role of `userId` in the project, or `null` when they are not in it. */
export function roleOf(
  members: readonly MemberView[],
  userId: string | null | undefined,
): ProjectRole | null {
  return members.find((member) => member.userId === userId)?.role ?? null;
}

/** May add people, remove them and switch `member` and `manager`. */
export function canManagePeople(role: ProjectRole | null): boolean {
  return role === "owner" || role === "manager";
}

export interface MemberActions {
  /** Switch between `member` and `manager`. */
  changeRole: boolean;
  remove: boolean;
  /** Remove oneself. */
  leave: boolean;
  /** Hand the project over to this person (the owner then becomes a manager). */
  makeOwner: boolean;
}

/** What `me` (with role `myRole`) may do to `target`. Nobody touches the owner. */
export function actionsOn(
  myRole: ProjectRole | null,
  me: string | null | undefined,
  target: MemberView,
): MemberActions {
  const self = target.userId === me;
  const owner = target.role === "owner";
  const manages = canManagePeople(myRole);
  return {
    changeRole: manages && !owner,
    remove: manages && !owner && !self,
    leave: self && !owner,
    makeOwner: myRole === "owner" && !self,
  };
}

export const ROLE_LABELS: Record<ProjectRole, string> = {
  owner: "Owner",
  manager: "Manager",
  member: "Member",
};
