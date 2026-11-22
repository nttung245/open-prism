import { describe, expect, it } from "vitest";
import type { MemberView } from "./api/types/MemberView";
import type { ProjectRole } from "./api/types/ProjectRole";
import { actionsOn, canManagePeople, roleOf } from "./members";

function member(userId: string, role: ProjectRole): MemberView {
  return {
    userId,
    username: userId,
    displayName: userId,
    role,
    addedAt: 0,
  };
}

const lan = member("lan", "owner");
const ha = member("ha", "manager");
const minh = member("minh", "member");
const people = [lan, ha, minh];

describe("roleOf", () => {
  it("finds the role of the person, or nothing", () => {
    expect(roleOf(people, "ha")).toBe("manager");
    expect(roleOf(people, "out")).toBeNull();
    expect(roleOf(people, null)).toBeNull();
  });
});

describe("actionsOn", () => {
  it("lets the owner do everything to others, and nothing to herself", () => {
    expect(actionsOn("owner", "lan", minh)).toEqual({
      changeRole: true,
      remove: true,
      leave: false,
      makeOwner: true,
    });
    expect(actionsOn("owner", "lan", lan)).toEqual({
      changeRole: false,
      remove: false,
      leave: false,
      makeOwner: false,
    });
  });

  it("lets a manager manage people but not the owner, and leave", () => {
    expect(actionsOn("manager", "ha", minh)).toMatchObject({
      changeRole: true,
      remove: true,
      makeOwner: false,
    });
    expect(actionsOn("manager", "ha", lan)).toEqual({
      changeRole: false,
      remove: false,
      leave: false,
      makeOwner: false,
    });
    expect(actionsOn("manager", "ha", ha)).toMatchObject({
      leave: true,
      remove: false,
    });
  });

  it("lets a member only leave", () => {
    for (const target of [lan, ha]) {
      expect(Object.values(actionsOn("member", "minh", target))).toEqual([
        false,
        false,
        false,
        false,
      ]);
    }
    expect(actionsOn("member", "minh", minh)).toEqual({
      changeRole: false,
      remove: false,
      leave: true,
      makeOwner: false,
    });
  });

  it("knows who manages people", () => {
    expect(
      ["owner", "manager", "member", null].map((role) =>
        canManagePeople(role as ProjectRole | null),
      ),
    ).toEqual([true, true, false, false]);
  });
});
