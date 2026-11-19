/** Images pasted or dropped into the editor, to turn into LaTeX. */
import type { EditorState } from "@codemirror/state";

/** Characters of text before / after the insertion point sent with a request. */
export const MAX_BEFORE = 1_500;
export const MAX_AFTER = 500;

/** The first image file of a drop, if any. */
export function droppedImage(
  data: Pick<DataTransfer, "files"> | null,
): File | null {
  return (
    Array.from(data?.files ?? []).find((file) =>
      file.type.startsWith("image/"),
    ) ?? null
  );
}

/** The image of a paste whose clipboard has an image and no text; otherwise `null`. */
export function pastedImage(
  data: Pick<DataTransfer, "files" | "getData"> | null,
): File | null {
  if (!data || data.getData("text/plain")) return null;
  return droppedImage(data);
}

/** The text sent with a request: up to 1,500 characters before `pos` and 500 after. */
export function textAroundPos(
  state: EditorState,
  pos: number,
): { before: string; after: string } {
  const before = state.doc.sliceString(Math.max(0, pos - MAX_BEFORE), pos);
  const after = state.doc.sliceString(
    pos,
    Math.min(state.doc.length, pos + MAX_AFTER),
  );
  // Never half of a character written with two UTF-16 units.
  return {
    before: /^[\uDC00-\uDFFF]/.test(before) ? before.slice(1) : before,
    after: /[\uD800-\uDBFF]$/.test(after) ? after.slice(0, -1) : after,
  };
}
