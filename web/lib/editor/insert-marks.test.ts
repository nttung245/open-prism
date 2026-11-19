import { history, undo } from "@codemirror/commands";
import { EditorSelection, EditorState } from "@codemirror/state";
import { describe, expect, it } from "vitest";
import {
  insertAt,
  insertMarks,
  markAt,
  markedPos,
  unmark,
} from "./insert-marks";

function stateWith(doc: string, cursor = 0) {
  return EditorState.create({
    doc,
    selection: EditorSelection.cursor(cursor),
    extensions: [insertMarks, history()],
  });
}

function marked(doc: string, pos: number, cursor = 0) {
  return stateWith(doc, cursor).update({ effects: markAt("m", pos) }).state;
}

/** Types `text` at `pos`, as the writer would. */
function typeAt(state: EditorState, pos: number, text: string) {
  return state.update({
    changes: { from: pos, insert: text },
    selection: EditorSelection.cursor(pos + text.length),
    userEvent: "input.type",
  }).state;
}

function undone(state: EditorState) {
  let result = state;
  undo({
    state,
    dispatch: (tr) => {
      result = tr.state;
    },
  });
  return result;
}

describe("insert marks", () => {
  it("follow text typed before them and stay put for text after", () => {
    let state = marked("Intro.\nResults.", 7);
    state = typeAt(state, 0, "New ");
    expect(markedPos(state, "m")).toBe(11);
    state = typeAt(state, state.doc.length, " More.");
    expect(markedPos(state, "m")).toBe(11);
    // Text typed exactly at the mark goes before the insertion point.
    state = typeAt(state, 11, "x");
    expect(markedPos(state, "m")).toBe(12);
  });

  it("are dropped when the text around them is deleted, and then insert at the cursor", () => {
    let state = marked("0123456789abcdef", 8, 2);
    state = state.update({ changes: { from: 5, to: 12 } }).state;
    expect(markedPos(state, "m")).toBeNull();
    state = state.update(insertAt(state, "m", "X")).state;
    expect(state.doc.toString()).toBe("01X234cdef");
    expect(state.selection.main.head).toBe(3);

    // Deleting the text just before or just after keeps the mark.
    let kept = marked("0123456789", 5);
    kept = kept.update({ changes: { from: 2, to: 5 } }).state;
    expect(markedPos(kept, "m")).toBe(2);
    kept = kept.update({ changes: { from: 2, to: 4 } }).state;
    expect(markedPos(kept, "m")).toBe(2);
  });

  it("insert as one undoable change, even right after typing", () => {
    let state = marked("Text.\n", 6);
    state = typeAt(state, 5, " More");
    state = state.update(insertAt(state, "m", "$x$")).state;
    expect(state.doc.toString()).toBe("Text. More\n$x$");
    expect(state.selection.main.head).toBe(state.doc.length);
    expect(markedPos(state, "m")).toBeNull();
    state = undone(state);
    expect(state.doc.toString()).toBe("Text. More\n");
  });

  it("put several lines on lines of their own", () => {
    const latex = "\\begin{equation}\n  x\n\\end{equation}";
    // In the middle of a line.
    let state = marked("before after", 7);
    state = state.update(insertAt(state, "m", latex)).state;
    expect(state.doc.toString()).toBe(`before \n${latex}\nafter`);
    expect(state.sliceDoc(state.selection.main.head - 14)).toBe(
      "\\end{equation}\nafter",
    );
    // On an empty or indented line: nothing to add.
    state = marked("a\n  \nb", 4);
    state = state.update(insertAt(state, "m", latex)).state;
    expect(state.doc.toString()).toBe(`a\n  ${latex}\nb`);
    // One line stays inline.
    state = marked("where  is", 6);
    state = state.update(insertAt(state, "m", "$x$")).state;
    expect(state.doc.toString()).toBe("where $x$ is");
  });

  it("can be forgotten", () => {
    let state = marked("abc", 1);
    state = state.update({ effects: unmark("m") }).state;
    expect(markedPos(state, "m")).toBeNull();
  });

  it("insert where the image was pasted after lines are added above (spec example)", () => {
    const lines = Array.from({ length: 30 }, (_, i) => `line ${i + 1}`);
    let state = stateWith(lines.join("\n"));
    const line20 = state.doc.line(20).from;
    state = state.update({ effects: markAt("m", line20) }).state;
    // Two lines typed at line 5 while the AI writes.
    state = typeAt(state, state.doc.line(5).from, "added one\nadded two\n");
    const typed = state.doc.toString();
    const latex = "\\begin{equation}\n  e^{-x^2}\n\\end{equation}";
    state = state.update(insertAt(state, "m", latex)).state;
    expect(state.doc.line(22).text).toBe("\\begin{equation}");
    expect(state.doc.line(25).text).toBe("line 20");
    state = undone(state);
    expect(state.doc.toString()).toBe(typed);
  });
});
