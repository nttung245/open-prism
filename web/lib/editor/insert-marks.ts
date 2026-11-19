/**
 * Positions remembered in a document to insert text there later, for example the LaTeX the AI
 * writes from a pasted image: each position follows the edits made meanwhile.
 */
import { isolateHistory } from "@codemirror/commands";
import {
  EditorSelection,
  type EditorState,
  MapMode,
  StateEffect,
  StateField,
  type TransactionSpec,
} from "@codemirror/state";

const addMark = StateEffect.define<{ id: string; pos: number }>();
const removeMark = StateEffect.define<string>();

/** The marks of a document, by id. A mark is dropped when text around it is deleted. */
export const insertMarks = StateField.define<ReadonlyMap<string, number>>({
  create: () => new Map(),
  update(marks, tr) {
    let next: Map<string, number> | null = null;
    if (tr.docChanged && marks.size > 0) {
      next = new Map();
      for (const [id, pos] of marks) {
        const mapped = tr.changes.mapPos(pos, 1, MapMode.TrackDel);
        if (mapped !== null) next.set(id, mapped);
      }
    }
    for (const effect of tr.effects) {
      if (effect.is(addMark)) {
        next ??= new Map(marks);
        next.set(effect.value.id, effect.value.pos);
      } else if (effect.is(removeMark)) {
        next ??= new Map(marks);
        next.delete(effect.value);
      }
    }
    return next ?? marks;
  },
});

/** The effect that remembers `pos` as the mark `id`. */
export function markAt(id: string, pos: number): StateEffect<unknown> {
  return addMark.of({ id, pos });
}

/** The effect that forgets the mark `id`. */
export function unmark(id: string): StateEffect<unknown> {
  return removeMark.of(id);
}

/** Where the mark `id` is now, or `null` if it is gone. */
export function markedPos(state: EditorState, id: string): number | null {
  return state.field(insertMarks, false)?.get(id) ?? null;
}

/**
 * Inserts `text` at the mark `id` (at the cursor if the mark is gone) as one undoable change,
 * and forgets the mark. Text of several lines goes on lines of its own. The cursor ends after
 * the text.
 */
export function insertAt(
  state: EditorState,
  id: string,
  text: string,
): TransactionSpec {
  const pos = markedPos(state, id) ?? state.selection.main.head;
  let insert = text;
  let start = pos;
  if (text.includes("\n")) {
    const line = state.doc.lineAt(pos);
    if (state.doc.sliceString(line.from, pos).trim()) {
      insert = `\n${insert}`;
      start += 1;
    }
    if (state.doc.sliceString(pos, line.to).trim()) insert = `${insert}\n`;
  }
  return {
    changes: { from: pos, insert },
    selection: EditorSelection.cursor(start + text.length),
    effects: removeMark.of(id),
    annotations: isolateHistory.of("full"),
    scrollIntoView: true,
    userEvent: "input.paste",
  };
}
