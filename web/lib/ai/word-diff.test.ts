import { describe, expect, it } from "vitest";
import { wordDiff } from "./word-diff";

describe("wordDiff", () => {
  it("marks changed words only", () => {
    expect(wordDiff("Teh results are good.", "The results are good.")).toEqual([
      { kind: "removed", text: "Teh" },
      { kind: "added", text: "The" },
      { kind: "same", text: " results are good." },
    ]);
  });

  it("keeps LaTeX commands as single tokens", () => {
    const segments = wordDiff("\\section{Intro}", "\\section{Introduction}");
    expect(segments).toContainEqual({ kind: "same", text: "\\section{" });
    expect(segments).toContainEqual({ kind: "removed", text: "Intro" });
    expect(segments).toContainEqual({ kind: "added", text: "Introduction" });
  });

  it("handles empty sides", () => {
    expect(wordDiff("", "new")).toEqual([{ kind: "added", text: "new" }]);
    expect(wordDiff("old", "")).toEqual([{ kind: "removed", text: "old" }]);
  });
});
