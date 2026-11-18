/** Rows of a unified diff, with line numbers and word highlights on changed line pairs. */
import { type Segment, wordDiff } from "@/lib/ai/word-diff";
import type { DiffHunk } from "@/lib/api/types/DiffHunk";

export type DiffRow = {
  kind: "context" | "added" | "removed";
  /** Line number in the older version (context and removed lines). */
  oldNumber: number | null;
  /** Line number in the newer version (context and added lines). */
  newNumber: number | null;
  /** The text split into unchanged and changed words; one segment when not paired. */
  segments: Segment[];
};

/**
 * Turns a hunk into rows. A run of removed lines followed by a run of added lines is paired
 * line by line, and each pair is compared word by word, so a small edit in a long line stands
 * out. Unpaired lines are shown whole.
 */
export function diffRows(hunk: DiffHunk): DiffRow[] {
  const rows: DiffRow[] = [];
  let oldNumber = hunk.oldStart;
  let newNumber = hunk.newStart;
  const lines = hunk.lines;
  let i = 0;
  while (i < lines.length) {
    const line = lines[i];
    if (line.kind === "context") {
      rows.push({
        kind: "context",
        oldNumber: oldNumber++,
        newNumber: newNumber++,
        segments: [{ kind: "same", text: line.text }],
      });
      i += 1;
      continue;
    }
    const removed: string[] = [];
    while (i < lines.length && lines[i].kind === "removed") {
      removed.push(lines[i].text);
      i += 1;
    }
    const added: string[] = [];
    while (i < lines.length && lines[i].kind === "added") {
      added.push(lines[i].text);
      i += 1;
    }
    const pairs = Math.min(removed.length, added.length);
    const removedRows: DiffRow[] = removed.map((text, n) => ({
      kind: "removed",
      oldNumber: oldNumber + n,
      newNumber: null,
      segments:
        n < pairs
          ? wordDiff(text, added[n]).filter((s) => s.kind !== "added")
          : [{ kind: "removed", text }],
    }));
    const addedRows: DiffRow[] = added.map((text, n) => ({
      kind: "added",
      oldNumber: null,
      newNumber: newNumber + n,
      segments:
        n < pairs
          ? wordDiff(removed[n], text).filter((s) => s.kind !== "removed")
          : [{ kind: "added", text }],
    }));
    oldNumber += removed.length;
    newNumber += added.length;
    rows.push(...removedRows, ...addedRows);
  }
  return rows;
}
