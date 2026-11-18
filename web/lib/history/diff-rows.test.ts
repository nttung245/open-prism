import { describe, expect, it } from "vitest";
import { diffRows } from "./diff-rows";

describe("diffRows", () => {
  it("numbers lines and pairs removed and added lines word by word", () => {
    const rows = diffRows({
      oldStart: 9,
      oldLines: 3,
      newStart: 9,
      newLines: 4,
      lines: [
        { kind: "context", text: "before" },
        { kind: "removed", text: "the quick fox" },
        { kind: "added", text: "the slow fox" },
        { kind: "added", text: "a new line" },
        { kind: "context", text: "after" },
      ],
    });
    expect(rows.map((r) => [r.kind, r.oldNumber, r.newNumber])).toEqual([
      ["context", 9, 9],
      ["removed", 10, null],
      ["added", null, 10],
      ["added", null, 11],
      ["context", 11, 12],
    ]);
    expect(rows[1].segments).toEqual([
      { kind: "same", text: "the " },
      { kind: "removed", text: "quick" },
      { kind: "same", text: " fox" },
    ]);
    expect(rows[2].segments).toEqual([
      { kind: "same", text: "the " },
      { kind: "added", text: "slow" },
      { kind: "same", text: " fox" },
    ]);
    // Not paired: shown whole.
    expect(rows[3].segments).toEqual([{ kind: "added", text: "a new line" }]);
  });

  it("handles removals without additions", () => {
    const rows = diffRows({
      oldStart: 1,
      oldLines: 2,
      newStart: 1,
      newLines: 0,
      lines: [
        { kind: "removed", text: "one" },
        { kind: "removed", text: "two" },
      ],
    });
    expect(rows.map((r) => [r.kind, r.oldNumber, r.segments])).toEqual([
      ["removed", 1, [{ kind: "removed", text: "one" }]],
      ["removed", 2, [{ kind: "removed", text: "two" }]],
    ]);
  });
});
