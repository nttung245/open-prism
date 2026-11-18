/** A word-level diff of two short texts, for showing what an AI edit changes. */

export type Segment = { kind: "same" | "removed" | "added"; text: string };

/** Words, whitespace runs and single punctuation marks. */
function tokenize(text: string): string[] {
  return text.match(/\s+|[\p{L}\p{N}_\\]+|[^\s\p{L}\p{N}_\\]/gu) ?? [];
}

const MAX_CELLS = 400_000;

export function wordDiff(before: string, after: string): Segment[] {
  const a = tokenize(before);
  const b = tokenize(after);
  if (a.length * b.length > MAX_CELLS) {
    return [
      ...(before ? [{ kind: "removed" as const, text: before }] : []),
      ...(after ? [{ kind: "added" as const, text: after }] : []),
    ];
  }
  // Longest common subsequence, filled from the end.
  const width = b.length + 1;
  const table = new Uint32Array((a.length + 1) * width);
  for (let i = a.length - 1; i >= 0; i--) {
    for (let j = b.length - 1; j >= 0; j--) {
      table[i * width + j] =
        a[i] === b[j]
          ? table[(i + 1) * width + j + 1] + 1
          : Math.max(table[(i + 1) * width + j], table[i * width + j + 1]);
    }
  }
  const segments: Segment[] = [];
  const push = (kind: Segment["kind"], text: string) => {
    const last = segments[segments.length - 1];
    if (last?.kind === kind) last.text += text;
    else segments.push({ kind, text });
  };
  let i = 0;
  let j = 0;
  while (i < a.length && j < b.length) {
    if (a[i] === b[j]) {
      push("same", a[i]);
      i++;
      j++;
    } else if (table[(i + 1) * width + j] >= table[i * width + j + 1]) {
      push("removed", a[i++]);
    } else {
      push("added", b[j++]);
    }
  }
  while (i < a.length) push("removed", a[i++]);
  while (j < b.length) push("added", b[j++]);
  return segments;
}
